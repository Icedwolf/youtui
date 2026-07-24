use crate::app::server::ArcServer;
use futures::{FutureExt, StreamExt};
use std::any::TypeId;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Discriminator {
    None,
    Kill(TypeId),
    BlockConcurrent(TypeId),
}

pub type MutationFn<C> = Box<dyn FnOnce(&mut C) -> Effects<C> + Send>;
type ExecFn<C> = Box<dyn FnOnce(&ArcServer) -> BoxFuture<MutationFn<C>> + Send>;

type StreamFn<C> = Box<dyn FnOnce(&ArcServer) -> Pin<Box<dyn futures::Stream<Item = MutationFn<C>> + Send>> + Send>;

enum WorkKind<C> {
    Single(ExecFn<C>),
    Stream(StreamFn<C>),
}

pub struct Work<C> {
    discriminator: Discriminator,
    kind: WorkKind<C>,
}

impl<C: 'static> Work<C> {
    fn map<C2>(self, f: impl Fn(&mut C2) -> &mut C + Clone + Send + 'static) -> Work<C2>
    where
        C: 'static,
    {
        let f2 = f.clone();
        let discriminator = self.discriminator;
        match self.kind {
            WorkKind::Single(execute) => Work {
                discriminator,
                kind: WorkKind::Single(Box::new(move |server: &ArcServer| {
                    let f = f.clone();
                    let server = Arc::clone(server);
                    Box::pin(async move {
                        let inner: MutationFn<C> = execute(&server).await;
                        let mapped: MutationFn<C2> =
                            Box::new(move |ui2: &mut C2| inner(f(ui2)).map(f.clone()));
                        mapped
                    })
                })),
            },
            WorkKind::Stream(stream_fn) => Work {
                discriminator,
                kind: WorkKind::Stream(Box::new(move |server: &ArcServer| {
                    let f = f2.clone();
                    let stream = stream_fn(server);
                    Box::pin(stream.map(move |mutation: MutationFn<C>| {
                        let f = f.clone();
                        let mapped: MutationFn<C2> =
                            Box::new(move |ui2: &mut C2| mutation(f(ui2)).map(f.clone()));
                        mapped
                    }))
                })),
            },
        }
    }
}

#[must_use]
pub struct Effects<C>(Vec<Work<C>>);

impl<C: 'static> Effects<C> {
    pub fn none() -> Self {
        Effects(vec![])
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn new<F, Fut>(build: F) -> Self
    where
        F: FnOnce(&ArcServer) -> Fut + Send + 'static,
        Fut: Future<Output = MutationFn<C>> + Send + 'static,
    {
        Effects(vec![Work {
            discriminator: Discriminator::None,
            kind: WorkKind::Single(Box::new(move |server| Box::pin(build(server)))),
        }])
    }

    pub fn new_stream<F, S>(build: F) -> Self
    where
        F: FnOnce(&ArcServer) -> S + Send + 'static,
        S: futures::Stream<Item = MutationFn<C>> + Send + 'static,
    {
        Effects(vec![Work {
            discriminator: Discriminator::None,
            kind: WorkKind::Stream(Box::new(move |server| {
                Box::pin(build(server))
            })),
        }])
    }

    pub fn kill_prev<D: 'static>(mut self) -> Self {
        if let Some(work) = self.0.last_mut() {
            work.discriminator = Discriminator::Kill(TypeId::of::<D>());
        }
        self
    }

    pub fn block_concurrent<D: 'static>(mut self) -> Self {
        if let Some(work) = self.0.last_mut() {
            work.discriminator = Discriminator::BlockConcurrent(TypeId::of::<D>());
        }
        self
    }

    pub fn push(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }

    pub fn map<C2>(self, f: impl Fn(&mut C2) -> &mut C + Clone + Send + 'static) -> Effects<C2>
    where
        C: 'static,
    {
        Effects(self.0.into_iter().map(|work| work.map(f.clone())).collect())
    }
}

pub enum TaskResult<C> {
    Mutation(Box<dyn FnOnce(&mut C) -> Effects<C> + Send>),
    StreamFinished,
    Panic(String),
}

pub struct TaskManager<C> {
    tokens: Arc<Mutex<HashMap<TypeId, CancellationToken>>>,
    result_tx: mpsc::UnboundedSender<TaskResult<C>>,
    result_rx: mpsc::UnboundedReceiver<TaskResult<C>>,
}

impl<C: 'static> TaskManager<C> {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        TaskManager {
            tokens: Arc::new(Mutex::new(HashMap::new())),
            result_tx: tx,
            result_rx: rx,
        }
    }

    pub fn spawn(&self, server: &ArcServer, effects: Effects<C>) {
        let tokens: Arc<Mutex<HashMap<TypeId, CancellationToken>>> = Arc::clone(&self.tokens);
        let result_tx: mpsc::UnboundedSender<TaskResult<C>> = self.result_tx.clone();

        for work in effects.0 {
            let tid_entry = match work.discriminator {
                Discriminator::None => None,
                Discriminator::Kill(tid) => {
                    let mut map = tokens.lock().unwrap();
                    if let Some(old_token) = map.remove(&tid) {
                        old_token.cancel();
                    }
                    let token = CancellationToken::new();
                    map.insert(tid, token);
                    Some(tid)
                }
                Discriminator::BlockConcurrent(tid) => {
                    let mut map = tokens.lock().unwrap();
                    if map.contains_key(&tid) {
                        debug!("Blocked concurrent task for {:?}", tid);
                        continue;
                    }
                    let token = CancellationToken::new();
                    map.insert(tid, token);
                    Some(tid)
                }
            };
            match work.kind {
                WorkKind::Single(execute) => spawn_work(
                    execute,
                    Arc::clone(server),
                    Arc::clone(&tokens),
                    result_tx.clone(),
                    tid_entry,
                ),
                WorkKind::Stream(stream_fn) => spawn_stream(
                    stream_fn,
                    Arc::clone(server),
                    Arc::clone(&tokens),
                    result_tx.clone(),
                    tid_entry,
                ),
            }
        }
    }

    pub async fn get_next_response(&mut self) -> Option<TaskResult<C>> {
        self.result_rx.recv().await
    }

}

fn spawn_stream<C: 'static>(
    stream_fn: StreamFn<C>,
    server: Arc<crate::app::server::Server>,
    tokens: Arc<Mutex<HashMap<TypeId, CancellationToken>>>,
    result_tx: mpsc::UnboundedSender<TaskResult<C>>,
    tid: Option<TypeId>,
) {
    tokio::spawn(async move {
        let mut stream = stream_fn(&server);
        while let Some(mutation) = stream.next().await {
            let _ = result_tx.send(TaskResult::Mutation(mutation));
        }
        let _ = result_tx.send(TaskResult::StreamFinished);
        if let Some(tid) = tid {
            tokens.lock().unwrap().remove(&tid);
        }
    });
}

fn spawn_work<C: 'static>(
    execute: ExecFn<C>,
    server: Arc<crate::app::server::Server>,
    tokens: Arc<Mutex<HashMap<TypeId, CancellationToken>>>,
    result_tx: mpsc::UnboundedSender<TaskResult<C>>,
    tid: Option<TypeId>,
) {
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(execute(&server))
            .catch_unwind()
            .await;
        match result {
            Ok(mutation) => {
                let _ = result_tx.send(TaskResult::Mutation(mutation));
            }
            Err(panic) => {
                let msg = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(|s: &String| s.as_str()))
                    .unwrap_or("unknown panic");
                error!("Background task panicked: {msg}");
                let _ = result_tx.send(TaskResult::Panic(msg.to_string()));
            }
        }
        if let Some(tid) = tid {
            tokens.lock().unwrap().remove(&tid);
        }
    });
}
