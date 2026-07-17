use std::any::TypeId;
use std::collections::HashMap;
use std::future::Future;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinError, JoinSet};
use tracing::warn;

use super::TaskMetadata;

pub enum TaskOutcome {
    Mutation(Box<dyn FnOnce(&mut super::YoutuiWindow) + Send>),
    Panicked { type_name: &'static str, error: JoinError },
    Finished,
}

enum Constraint {
    KillSameType,
    BlockSameType,
    BlockMatchingMetadata(TaskMetadata),
}

struct TrackedTask {
    type_id: TypeId,
    type_name: &'static str,
    metadata: Vec<TaskMetadata>,
    abort: AbortHandle,
}

pub struct TaskManager {
    active: Vec<TrackedTask>,
    result_tx: mpsc::UnboundedSender<TaskOutcome>,
    pub result_rx: mpsc::UnboundedReceiver<TaskOutcome>,
    spawn_handle: Option<AbortHandle>,
}

impl TaskManager {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        Self {
            active: Vec::new(),
            result_tx,
            result_rx,
            spawn_handle: None,
        }
    }

    fn apply_constraint(&mut self, constraint: &Constraint, type_id: TypeId) {
        match constraint {
            Constraint::KillSameType => {
                self.active.retain_mut(|t| {
                    if t.type_id == type_id {
                        t.abort.abort();
                        false
                    } else {
                        true
                    }
                });
            }
            Constraint::BlockSameType => {
                self.active.retain(|t| t.type_id != type_id);
            }
            Constraint::BlockMatchingMetadata(meta) => {
                self.active.retain(|t| {
                    if t.type_id == type_id && t.metadata.contains(meta) {
                        t.abort.abort();
                        false
                    } else {
                        true
                    }
                });
            }
        }
    }

    pub fn spawn_future(
        &mut self,
        type_id: TypeId,
        type_name: &'static str,
        metadata: Vec<TaskMetadata>,
        constraint: Option<&super::server::task_manager::Constraint>,
        future: impl Future<Output = Option<Box<dyn FnOnce(&mut super::YoutuiWindow) + Send>>> + Send + 'static,
    ) {
    }
}
