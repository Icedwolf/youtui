use super::*;
use futures::FutureExt;

pub fn playback_stream(
    server: crate::app::server::ArcServer,
    id: ListSongID,
    decoder: Box<dyn Source<Item = f32> + Send + 'static>,
) -> impl Stream<Item = MutationFn<Playlist>> + Send + 'static {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let play_stream = server.player.play_song(decoder, id);

    tokio::spawn(async move {
        let mut play_stream = play_stream;
        while let Some(update) = play_stream.next().await {
            let mutation: MutationFn<Playlist> = match update {
                PlayUpdate::PlayProgress(d, id) => {
                    Box::new(move |this: &mut Playlist| this.handle_set_song_play_progress(d, id))
                }
                PlayUpdate::Playing(d, id) => {
                    Box::new(move |this: &mut Playlist| this.handle_playing(d, id))
                }
                PlayUpdate::DonePlaying(id) => {
                    Box::new(move |this: &mut Playlist| this.handle_done_playing(id))
                }
            };
            let _ = tx.send(mutation);
        }
    });

    UnboundedReceiverStream::new(rx)
}

impl Playlist {
    pub fn new(volume: Percentage) -> (Self, Effects<Self>) {
        let task = Effects::none();

        let playlist = Playlist {
            volume,
            play_status: PlayState::NotPlaying,
            list: Default::default(),
            id_to_index_cache: HashMap::new(),
            cur_played_dur: None,
            cur_selected: 0,
            queue_status: QueueState::NotQueued,
            widget_state: Default::default(),
            shuffle_enabled: false,
            shuffle_indices: Vec::new(),
            shuffle_visual_map: Vec::new(),
            shuffle_seed: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            active_downloads: Arc::new(std::sync::Mutex::new(Vec::new())),
            download_queue: VecDeque::new(),
            search_enabled: false,
            search_text: String::new(),
            search_indices: Vec::new(),
            search_visual_map: Vec::new(),
            pre_search_selected: 0,
            loaded_from_autosave: false,
            preloaded_sources: HashMap::new(),
            play_next_queue: VecDeque::new(),
            resolving_audio: false,
            resolve_remaining: 0,
            cached_title: RefCell::new(None),
            notifications_enabled: true,
            auth_notif_last: None,
            consecutive_download_failures: 0,
            shuffle_regen_token: None,
        };

        (playlist, task)
    }

    pub fn set_notifications_enabled(&mut self, enabled: bool) {
        self.notifications_enabled = enabled;
    }

    /// Notify the user that songs are being skipped due to a stale YouTube
    /// login, debounced by `AUTH_ERROR_NOTIF_COOLDOWN` so a queue of failed
    /// songs produces one popup, not one per song. `detail` is the underlying
    /// yt-dlp error line, used to make the message diagnosable.
    fn notify_auth_error(&mut self, detail: &str) {
        let now = std::time::Instant::now();
        if let Some(last) = self.auth_notif_last
            && now.duration_since(last) < AUTH_ERROR_NOTIF_COOLDOWN
        {
            return;
        }
        self.auth_notif_last = Some(now);
        if !self.notifications_enabled {
            return;
        }
        let clipped: String = detail.chars().take(200).collect();
        let body = format!(
            "{clipped} — your YouTube login looks stale and songs are being skipped. \
Re-log into your browser, or refresh your cookie file / po_token, then restart."
        );
        spawn_notification("YouTube Authentication Issue", &body, 8000);
    }

    /// Park the player and stop the queue after a run of consecutive download
    /// failures of the currently-buffering song. A systemic failure (dead
    /// yt-dlp, exhausted resources) would otherwise walk the whole queue,
    /// skipping/removing every song. Keeps the list intact; the user can
    /// retry a song manually once the root cause is addressed.
    fn halt_on_download_failures(&mut self, detail: &str) {
        warn!(
            "Halting playback after {} consecutive download failures: {}",
            HALT_AFTER_CONSECUTIVE_FAILURES, detail
        );
        self.play_status = PlayState::NotPlaying;
        self.queue_status = QueueState::NotQueued;
        self.cancel_all_downloads();
        self.consecutive_download_failures = 0;
        if !self.notifications_enabled {
            return;
        }
        let clipped: String = detail.chars().take(200).collect();
        let body = format!(
            "{clipped} — {N} consecutive downloads failed and playback was halted. \
             Check your network / yt-dlp install, then play a song again.",
            N = HALT_AFTER_CONSECUTIVE_FAILURES
        );
        spawn_notification("Download Failures", &body, 8000);
    }

    pub fn volume(&self) -> Percentage {
        self.volume
    }

    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle_enabled
    }

    pub fn stop_song_id(&self, song_id: ListSongID) -> Effects<Self> {
        Effects::new(move |_: &crate::app::server::ArcServer| async move {
            Box::new(move |this: &mut Playlist| {
                this.handle_stopped(Stopped(song_id));
                Effects::none()
            }) as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
        })
    }

    pub fn get_cur_played_dur(&self) -> Option<Duration> {
        self.cur_played_dur
    }

    fn prepare_playback_id(&mut self, id: ListSongID) -> Effects<Self> {
        self.drop_unscoped_from_id(id);
        self.cur_played_dur = None;
        Effects::none()
    }

    fn start_buffering(
        &mut self,
        id: ListSongID,
        stop_song_id: Option<ListSongID>,
        mut effect: Effects<Self>,
    ) -> Effects<Self> {
        self.play_status = PlayState::Buffering(id);
        self.queue_status = QueueState::NotQueued;
        if let Some(cur_id) = stop_song_id {
            effect = effect.push(self.stop_song_id(cur_id));
        }
        effect
    }

    pub fn play_song(&mut self, id: ListSongID) -> Effects<Self> {
        if let Some(idx) = self.get_index_from_id(id)
            && let Some(song) = self.list.get_list_iter_mut().nth(idx)
            && matches!(song.download_status, DownloadStatus::Failed)
        {
            song.download_status = DownloadStatus::None;
        }
        let mut effect = self.prepare_playback_id(id);

        if let Some(song_index) = self.get_index_from_id(id) {
            let stop_song_id = self.get_cur_playing_id();
            effect = self.start_buffering(id, stop_song_id, effect);

            let video_id = self
                .list
                .get_list_iter()
                .nth(song_index)
                .map(|s| s.video_id.get_raw().to_string());
            let cache_decoder = video_id
                .as_deref()
                .and_then(create_decoder_from_cache)
                .map(|d| Box::new(d) as Box<dyn Source<Item = f32> + Send + 'static>);

            if let Some(decoder) = cache_decoder {
                let task = Effects::new_stream(
                    move |server: &crate::app::server::ArcServer| {
                        playback_stream(Arc::clone(server), id, decoder)
                    }
                );
                effect = effect.push(task);
            } else if let Some(preloaded) = self.preloaded_sources.remove(&id) {
                let task = Effects::new_stream(
                    move |server: &crate::app::server::ArcServer| {
                        playback_stream(Arc::clone(server), id, preloaded)
                    }
                );
                effect = effect.push(task);
            } else {
                effect = effect.push(
                    Effects::new(|server: &crate::app::server::ArcServer| {
                        server.player.stop();
                        async move {
                            Box::new(|_: &mut Playlist| Effects::none())
                                as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
                        }
                    })
                );
                effect = effect.push(self.download_song(id));
            }
        } else {
            debug!("play_song called with unknown id {id:?}");
        }
        effect
    }

    pub fn reset(&mut self) -> Effects<Self> {
        let mut effect = Effects::none();

        if let Some(cur_id) = self.get_cur_playing_id() {
            effect = self.stop_song_id(cur_id);
        }

        self.cancel_all_downloads();
        self.clear();
        effect
    }

    pub fn deduplicate(&mut self) {
        let removed = self.list.deduplicate();
        if removed > 0 {
            debug!(%removed, "Removed duplicate songs from playlist");
        }
        self.rebuild_id_cache();
        if removed > 0 {
            if self.shuffle_enabled {
                self.generate_shuffle_indices();
            }
            self.update_search_indices();
            self.cached_title.borrow_mut().take();
        }
    }

    pub fn shuffle_seed(&self) -> u64 {
        self.shuffle_seed
    }

    pub fn loaded_from_autosave(&self) -> bool {
        self.loaded_from_autosave
    }

    pub fn set_loaded_from_autosave(&mut self, v: bool) {
        self.loaded_from_autosave = v;
    }

    pub fn enable_shuffle(&mut self, seed: u64) {
        self.shuffle_enabled = true;
        self.shuffle_seed = seed;
        self.generate_shuffle_indices();
        if let (Some(_current_id), Some(playing_idx)) =
            (self.get_cur_playing_id(), self.get_cur_playing_index())
        {
            if let Some(shuffled_pos) =
                self.shuffle_visual_map
                    .get(playing_idx)
                    .copied()
                    .flatten()
            {
                self.cur_selected = shuffled_pos;
            }
        } else {
            self.cur_selected = 0.min(self.get_max_visual_index());
        }
    }

    pub fn clear(&mut self) {
        self.cur_played_dur = None;
        self.play_status = PlayState::NotPlaying;
        self.list.clear();
        self.id_to_index_cache.clear();
        self.shuffle_indices.clear();
        self.shuffle_visual_map.clear();
        self.search_indices.clear();
        self.search_visual_map.clear();
        self.download_queue.clear();
        self.play_next_queue.clear();
        self.preloaded_sources.clear();
        self.cur_selected = 0;
        cache_clear();
        self.cached_title.borrow_mut().take();
    }

    pub fn play_prev(&mut self) -> Effects<Self> {
        let cur = &self.play_status;
        match cur {
            PlayState::NotPlaying  => {
                debug!("play_prev: stopped, jumping to last song");
                let last_visual = self.get_max_visual_index();
                let last_actual = self.visual_to_actual_index(last_visual);
                if let Some(last_id) = self.get_id_from_index(last_actual) {
                    self.cur_selected = last_visual;
                    self.play_song(last_id)
                } else {
                    debug!("play_prev: queue is empty");
                    Effects::none()
                }
            }
            PlayState::Paused(_)
            | PlayState::Playing(_)
            | PlayState::Buffering(_)
            | PlayState::Error(_) => {
                if let Some(prev_song_id) = self.get_prev_song_id() {
                    self.play_song(prev_song_id)
                } else {
                    debug!("play_prev: at first song, wrapping to last");
                    let last_visual = self.get_max_visual_index();
                    let last_actual = self.visual_to_actual_index(last_visual);
                    if let Some(last_id) = self.get_id_from_index(last_actual) {
                        self.cur_selected = last_visual;
                        self.play_song(last_id)
                    } else {
                        Effects::none()
                    }
                }
            }
        }
    }

    pub fn handle_song_downloaded(&mut self, id: ListSongID) -> Effects<Self> {
        let start = std::time::Instant::now();
        if let PlayState::Buffering(target_id) = self.play_status
            && target_id == id
        {
            debug!(
                "play_attempt: song_id={:?}, state=Buffering, ms_since_download={}",
                id,
                start.elapsed().as_millis()
            );
            if matches!(self.queue_status, QueueState::Queued(_)) {
                debug!(
                    "autoplay_started: song_id={:?}, ms_to_start={}",
                    id,
                    start.elapsed().as_millis()
                );
            } else {
                debug!(
                    "play_started: song_id={:?}, ms_to_start={}",
                    id,
                    start.elapsed().as_millis()
                );
            }
            return Effects::none();
        }
        debug!(
            "download_handled_not_playing: song_id={:?}, state={:?}",
            id, self.play_status
        );
        Effects::none()
    }

    pub fn increase_volume(&mut self, inc: i8) {
        self.volume.0 = self.volume.0.saturating_add_signed(inc).clamp(0, 100);
    }

    pub fn set_volume(&mut self, new_vol: u8) {
        self.volume.0 = new_vol.clamp(0, 100);
    }

    pub fn push_song_list(
        &mut self,
        song_list: Vec<ListSong>,
    ) -> (ListSongID, Effects<Self>) {
        let was_playing = self.get_cur_playing_id();
        let first_id = self.list.push_song_list(song_list);
        self.rebuild_id_cache();

        if self.shuffle_enabled {
            self.generate_shuffle_indices();

            if let (_, Some(playing_idx)) =
                (was_playing, self.get_cur_playing_index())
            {
                if let Some(shuffled_pos) =
                    self.shuffle_indices.iter().position(|&i| i == playing_idx)
                {
                    self.cur_selected = shuffled_pos;
                }
            } else {
                self.cur_selected = 0.min(self.get_max_visual_index());
            }
        }

        if !self.search_text.is_empty() {
            self.update_search_indices();
            self.cur_selected = self.cur_selected.min(self.get_max_visual_index());
        }

        self.cached_title.borrow_mut().take();
        (first_id, Effects::none())
    }

    pub fn play_next_or_stop(&mut self, prev_id: ListSongID) -> Effects<Self> {
        self.play_next_inner(prev_id, "finishing playback")
    }

    pub fn autoplay_next_or_stop(&mut self, prev_id: ListSongID) -> Effects<Self> {
        self.play_next_inner(prev_id, "resetting play status")
    }

    fn reshuffle_or_wrap(&mut self) -> usize {
        if self.shuffle_enabled {
            self.shuffle_seed = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            self.generate_shuffle_indices();
        }
        0
    }

    fn play_next_inner(
        &mut self,
        prev_id: ListSongID,
        no_next_msg: &str,
    ) -> Effects<Self> {
        let current_id = self.get_cur_playing_id();
        match &self.play_status {
            PlayState::NotPlaying  => {
                debug!("Asked to play next, but not currently playing");
                Effects::none()
            }
            PlayState::Paused(_)
            | PlayState::Playing(_)
            | PlayState::Buffering(_)
            | PlayState::Error(_) => {
                let Some(id) = current_id else {
                    return Effects::none();
                };
                if id > prev_id {
                    debug!(
                        "play_next_inner: newer song already playing (id={id:?} > prev={prev_id:?})"
                    );
                    return Effects::none();
                }

                if let Some(next_id) = self.play_next_queue.pop_front() {
                    self.cached_title.borrow_mut().take();
                    return self.play_song(next_id);
                }

                if let Some(next_song_id) = self.get_next_song_id() {
                    self.play_song(next_song_id)
                } else {
                    debug!("No next song - {no_next_msg}, reshuffling/wrapping");
                    self.reshuffle_or_wrap();
                    if let Some(first_id) = self.first_live_song_id() {
                        let first_visual = self
                            .get_index_from_id(first_id)
                            .and_then(|idx| self.actual_to_visual_index(idx))
                            .unwrap_or(0);
                        self.cur_selected = first_visual;
                        self.play_song(first_id)
                    } else {
                        self.play_status = PlayState::NotPlaying;
                        self.queue_status = QueueState::NotQueued;
                        self.preloaded_sources.clear();
                        self.stop_song_id(id)
                    }
                }
            }
        }
    }

    pub fn download_upcoming_from_id(&mut self, id: ListSongID) -> Effects<Self> {
        let Some(song_index) = self.get_index_from_id(id) else {
            return Effects::none();
        };

        debug!(
            "download_upcoming_from_id: START for id={:?}, index={}",
            id, song_index
        );

        let mut song_ids: Vec<ListSongID> = Vec::new();

        for pn_id in self.play_next_queue.iter().take(SONGS_AHEAD_TO_BUFFER) {
            if !song_ids.contains(pn_id) && self.get_index_from_id(*pn_id).is_some() {
                song_ids.push(*pn_id);
            }
        }

        if self.shuffle_enabled {
            let Some(visual_index) = self.actual_to_visual_index(song_index) else {
                return Effects::none();
            };

            for offset in 1..=SONGS_AHEAD_TO_BUFFER {
                let next_pos = visual_index.saturating_add(offset);
                if next_pos < self.shuffle_indices.len() {
                    let next_actual = self.shuffle_indices[next_pos];
                    if let Some(next_id) = self.get_id_from_index(next_actual)
                        && !song_ids.contains(&next_id)
                    {
                        song_ids.push(next_id);
                    }
                }
            }
        } else {
            for offset in 1..=SONGS_AHEAD_TO_BUFFER {
                if let Some(next_song) = self.get_song_from_idx(song_index.saturating_add(offset))
                    && !song_ids.contains(&next_song.id)
                {
                    song_ids.push(next_song.id);
                }
            }
        }

        for &sid in &song_ids {
            if let Some(idx) = self.get_index_from_id(sid)
                && let Some(s) = self.list.get_list_iter().nth(idx)
            {
                debug!(
                    "  scope_song: id={:?}, video_id={}, status={:?}",
                    sid,
                    s.video_id.get_raw(),
                    s.download_status
                );
            }
        }

        let mut cancel_scope = vec![id];
        cancel_scope.extend(&song_ids);
        self.cancel_out_of_scope_downloads(&cancel_scope);

        debug!(
            "download_upcoming_from_id: queue BEFORE clear: {:?}",
            self.download_queue
        );

        self.download_queue.clear();
        for song_id in &song_ids {
            let status = if let Some(idx) = self.get_index_from_id(*song_id) {
                self.get_song_from_idx(idx).map(|s| &s.download_status)
            } else {
                None
            };

            match status {
                Some(DownloadStatus::Downloaded) => {
                    debug!(
                        "download_upcoming_from_id: skipping {:?} (already downloaded)",
                        song_id
                    );
                }
                Some(DownloadStatus::Failed) => {
                    debug!(
                        "download_upcoming_from_id: skipping {:?} (previously failed)",
                        song_id
                    );
                }
                _ => {
                    self.download_queue.push_back(*song_id);
                }
            }
        }

        debug!(
            "download_upcoming_from_id: queue AFTER filtering: {:?}",
            self.download_queue
        );

        let mut combined_effect = Effects::none();
        if let Some(first_id) = self.download_queue.pop_front() {
            debug!(
                "download_upcoming_from_id: STARTING FIRST DOWNLOAD: {:?}",
                first_id
            );
            combined_effect = combined_effect.push(self.download_song(first_id));
        } else {
            debug!(
                "download_upcoming_from_id: no download needed (all in scope already downloaded)"
            );
        }

        combined_effect
    }

    pub fn download_song(&mut self, id: ListSongID) -> Effects<Self> {
        let Some(song_index) = self.get_index_from_id(id) else {
            debug!("download_song: song id {:?} not found", id);
            self.play_status = PlayState::NotPlaying;
            return Effects::none();
        };

        let song = match self.list.get_list_iter_mut().nth(song_index) {
            Some(s) => s,
            None => {
                debug!(
                    "download_song: index {} for id {:?} out of bounds after getting index",
                    song_index, id
                );
                self.play_status = PlayState::NotPlaying;
                return Effects::none();
            }
        };

        let video_id = song.video_id.get_raw().to_string();
        debug!("download_song: {}", video_id);

        match &song.download_status {
            DownloadStatus::Downloading(_) => {
                debug!("download_song: {} already downloading", video_id);
                return Effects::none();
            }
            DownloadStatus::Failed => {
                debug!(
                    "download_song: {} previously failed — popping from queue",
                    video_id
                );
                self.download_queue.pop_front();
                if let Some(next_id) = self.download_queue.front().copied() {
                    return self.download_song(next_id);
                }
                debug!(
                    "download_song: {} failed and no queued songs remaining",
                    video_id
                );
                self.play_status = PlayState::NotPlaying;
                return Effects::none();
            }
            DownloadStatus::None => {}
            DownloadStatus::Queued => {
                debug!("download_song: {} queued — proceeding with download", video_id);
            }
            DownloadStatus::Downloaded => {
                warn!("download_song: {} already downloaded — re-downloading", video_id);
            }
        };

        let cancel_token = Arc::new(tokio_util::sync::CancellationToken::new());
        let cancel_token_for_stream = cancel_token.clone();

        let mut downloads = self.active_downloads.lock().unwrap_or_warn();
        if downloads.iter().any(|(sid, task)| *sid == id && !task.cancel_token.is_cancelled()) {
            if matches!(song.download_status, DownloadStatus::Queued) {
                debug!(
                    "download_song: {} already queued with active download",
                    video_id
                );
            } else {
                debug!("download_song: {} already downloading", video_id);
            }
            return Effects::none();
        }
        if matches!(song.download_status, DownloadStatus::Queued) {
            warn!(
                "download_song: {} status=Queued but no active download — possible invariant violation, proceeding",
                video_id
            );
        }
        let idx = downloads.iter().position(|(sid, _)| *sid == id);
        if let Some(i) = idx {
            downloads[i].1.cancel_token.cancel();
            downloads.swap_remove(i);
        }

        debug!("download_song: starting download for {}", video_id);

        let effect = Effects::new_stream(
            move |server: &crate::app::server::ArcServer| {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let yt_cmd = server.config.yt_dlp_command.clone();
                let pt = server.po_token.clone();
                let cp = server.cookie_path.clone();
                let ch = server.cookie_header.clone();
                let jr = server.js_runtime.clone();
                let vid = video_id.clone();
                let song_id = id;

                tokio::spawn(async move {
                    let emit: MutationFn<Playlist> = Box::new(move |this: &mut Playlist| {
                        this.handle_song_download_progress_update(
                            DownloadProgressUpdate::Downloading, song_id,
                        )
                    });
                    let _ = tx.send(emit);

                    let result = std::panic::AssertUnwindSafe(
                        download_and_decode(crate::app::server::song_downloader::DownloadConfig {
                            yt_dlp_command: yt_cmd,
                            video_id: vid,
                            po_token: pt,
                            cookie_path: cp,
                            cookie_header: ch,
                            js_runtime: jr,
                            cancel_token: (*cancel_token_for_stream).clone(),
                        }),
                    )
                    .catch_unwind()
                    .await;

                    let emit: MutationFn<Playlist> = match result {
                        Ok(Ok(decoder)) => {
                            let decoder = Box::new(decoder)
                                as Box<dyn Source<Item = f32> + Send>;
                            Box::new(move |this: &mut Playlist| {
                                this.handle_song_download_progress_update(
                                    DownloadProgressUpdate::Completed(decoder), song_id,
                                )
                            })
                        }
                        Ok(Err(e)) => {
                            let err = e.to_string();
                            Box::new(move |this: &mut Playlist| {
                                this.handle_song_download_progress_update(
                                    DownloadProgressUpdate::Error(err), song_id,
                                )
                            })
                        }
                        Err(panic) => {
                            let msg = crate::core::panic_message(&panic);
                            error!("download_and_decode panicked: {msg}");
                            Box::new(move |this: &mut Playlist| {
                                this.handle_song_download_progress_update(
                                    DownloadProgressUpdate::Error(format!("download panicked: {msg}")),
                                    song_id,
                                )
                            })
                        }
                    };
                    let _ = tx.send(emit);
                });

                UnboundedReceiverStream::new(rx)
            }
        );

        downloads.push((id, DownloadTask { cancel_token }));

        song.download_status = DownloadStatus::Queued;
        effect
    }

    pub fn drop_unscoped_from_id(&mut self, id: ListSongID) {
        let Some(song_index) = self.get_index_from_id(id) else {
            return;
        };

        let forward_limit = song_index.saturating_add(SONGS_AHEAD_TO_BUFFER);
        let backwards_limit = song_index.saturating_sub(SONGS_BEHIND_TO_SAVE);

        let mut downloads = self.active_downloads.lock().unwrap_or_warn();
        downloads.retain(|(song_id, task)| {
            if let Some(idx) = self.get_index_from_id(*song_id)
                && (idx < backwards_limit || idx >= forward_limit)
                && !self.play_next_queue.contains(song_id)
            {
                task.cancel_token.cancel();
                return false;
            }
            true
        });

        for (idx, song) in self.list.get_list_iter_mut().enumerate() {
            if (idx < backwards_limit || idx >= forward_limit)
                && !self.play_next_queue.contains(&song.id)
            {
                song.download_status = DownloadStatus::None;
            }
        }

        let out_of_scope: Vec<ListSongID> = self
            .preloaded_sources
            .keys()
            .filter(|id| {
                self.get_index_from_id(**id)
                    .is_none_or(|idx| idx < backwards_limit || idx >= forward_limit)
            })
            .filter(|id| !self.play_next_queue.contains(id))
            .copied()
            .collect();
        for id in out_of_scope {
            self.preloaded_sources.remove(&id);
        }
    }

    pub fn get_cur_playing_id(&self) -> Option<ListSongID> {
        match self.play_status {
            PlayState::Error(id)
            | PlayState::Playing(id)
            | PlayState::Paused(id)
            | PlayState::Buffering(id) => Some(id),
            PlayState::NotPlaying  => None,
        }
    }

    pub fn get_cur_playing_song(&self) -> Option<&ListSong> {
        self.get_cur_playing_id()
            .and_then(|id| self.get_song_from_id(id))
    }

    pub fn status_bar_icon(&self) -> char {
        match self.play_status {
            PlayState::Playing(id) => {
                if self
                    .get_song_from_id(id)
                    .is_some_and(|s| !matches!(s.download_status, DownloadStatus::Downloaded))
                {
                    ''
                } else {
                    ''
                }
            }
            PlayState::Buffering(_) => '',
            PlayState::Paused(_) => '',
            PlayState::Error(_) => '',
            PlayState::NotPlaying  => '',
        }
    }

    pub fn get_next_song(&self) -> Option<&ListSong> {
        self.get_cur_playing_id()
            .and_then(|_| self.get_next_song_id())
            .and_then(|next_id| self.get_song_from_id(next_id))
    }

    fn rebuild_id_cache(&mut self) {
        self.id_to_index_cache = self
            .list
            .get_list_iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
    }

    pub fn get_index_from_id(&self, id: ListSongID) -> Option<usize> {
        self.id_to_index_cache
            .get(&id)
            .copied()
            .or_else(|| self.list.get_list_iter().position(|s| s.id == id))
    }

    pub fn get_id_from_index(&self, index: usize) -> Option<ListSongID> {
        self.get_song_from_idx(index).map(|s| s.id)
    }

    pub fn get_mut_song_from_id(&mut self, id: ListSongID) -> Option<&mut ListSong> {
        let idx = self.get_index_from_id(id)?;
        self.list.get_list_iter_mut().nth(idx)
    }

    pub fn get_song_from_id(&self, id: ListSongID) -> Option<&ListSong> {
        let idx = self.get_index_from_id(id)?;
        self.list.get_list_iter().nth(idx)
    }

    pub fn check_id_is_cur(&self, check_id: ListSongID) -> bool {
        self.get_cur_playing_id().is_some_and(|id| id == check_id)
    }

    pub fn get_cur_playing_index(&self) -> Option<usize> {
        self.get_cur_playing_id()
            .and_then(|id| self.get_index_from_id(id))
    }
    pub fn go_to_first(&mut self) {
        self.cur_selected = 0;
    }

    pub fn go_to_last(&mut self) {
        self.cur_selected = self.list.get_list_iter().len().saturating_sub(1);
    }
}

impl Playlist {
    pub async fn handle_tick(&mut self) {}

    pub fn handle_next(&mut self) -> Effects<Self> {
        match self.play_status {
            PlayState::NotPlaying  => {
                debug!("Asked to play next, but not currently playing");
                Effects::none()
            }
            PlayState::Paused(id)
            | PlayState::Playing(id)
            | PlayState::Buffering(id)
            | PlayState::Error(id) => self.play_next_or_stop(id),
        }
    }

    pub fn handle_previous(&mut self) -> Effects<Self> {
        self.play_prev()
    }

    pub fn pauseplay(&mut self) -> Effects<Self> {
        let _id = match self.play_status {
            PlayState::Playing(id) => {
                self.play_status = PlayState::Paused(id);
                id
            }
            PlayState::Paused(id) => {
                self.play_status = PlayState::Playing(id);
                id
            }
            _ => return Effects::none(),
        };

        Effects::new(|server: &crate::app::server::ArcServer| {
            server.player.pause();
            async move {
                Box::new(|_: &mut Playlist| Effects::none()) as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
            }
        })
    }

    pub fn resume(&mut self) -> Effects<Self> {
        let _id = match self.play_status {
            PlayState::Paused(id) => {
                self.play_status = PlayState::Playing(id);
                id
            }
            _ => return Effects::none(),
        };

        Effects::new(|server: &crate::app::server::ArcServer| {
            server.player.pause();
            async move {
                Box::new(|_: &mut Playlist| Effects::none()) as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
            }
        })
    }

    pub fn pause(&mut self) -> Effects<Self> {
        let _id = match self.play_status {
            PlayState::Playing(id) => {
                self.play_status = PlayState::Paused(id);
                id
            }
            _ => return Effects::none(),
        };

        Effects::new(|server: &crate::app::server::ArcServer| {
            server.player.pause();
            async move {
                Box::new(|_: &mut Playlist| Effects::none()) as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
            }
        })
    }

    pub fn stop(&mut self) -> Effects<Self> {
        self.play_status = PlayState::NotPlaying;
        self.preloaded_sources.clear();
        cache_clear();
        self.cancel_all_downloads();
        Effects::new(|server: &crate::app::server::ArcServer| {
            server.player.stop();
            async move {
                Box::new(move |this: &mut Playlist| {
                    this.handle_all_stopped(AllStopped);
                    Effects::none()
                }) as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
            }
        })
    }

    pub fn play_selected(&mut self) -> Effects<Self> {
        if !self.search_text.is_empty() && self.search_indices.is_empty() {
            return Effects::none();
        }
        if self.list.get_list_iter().len() == 0 {
            return Effects::none();
        }

        let actual_index = self.visual_to_actual_index(self.cur_selected);
        let Some(id) = self.get_id_from_index(actual_index) else {
            return Effects::none();
        };
        self.play_song(id)
    }

    pub fn delete_selected(&mut self) -> Effects<Self> {
        if !self.search_text.is_empty() && self.search_indices.is_empty() {
            return Effects::none();
        }

        let mut return_task = Effects::none();

        if self.list.get_list_iter().len() == 0 {
            return return_task;
        }

        let visual_index_before = self.cur_selected;
        let actual_index = self.visual_to_actual_index(visual_index_before);
        let deleted_id = self.get_id_from_index(actual_index);

        if let Some(cur_playing_id) = self.get_cur_playing_id()
            && Some(actual_index) == self.get_cur_playing_index()
        {
            self.play_status = PlayState::NotPlaying;
            return_task = self.stop_song_id(cur_playing_id);
        }

        if let Some(id) = deleted_id {
            self.cancel_song_download(id);
            self.download_queue.retain(|qid| *qid != id);
        }

        self.list.remove_song_index(actual_index);
        self.rebuild_id_cache();

        if !self.search_text.is_empty() {
            self.update_search_indices();
            self.cur_selected = if self.search_indices.is_empty() {
                0
            } else {
                visual_index_before.min(self.search_indices.len() - 1)
            };
        } else {
            let new_max = self.list.get_list_iter().len().saturating_sub(1);
            self.cur_selected = self.cur_selected.min(new_max);
        }

        if self.shuffle_enabled {
            if let Some(pos) = self.shuffle_indices.iter().position(|&i| i == actual_index) {
                self.shuffle_indices.remove(pos);
            }
            for idx in &mut self.shuffle_indices {
                if *idx > actual_index {
                    *idx = idx.saturating_sub(1);
                }
            }
            self.shuffle_visual_map =
                build_visual_map(&self.shuffle_indices, self.list.get_list_iter().count());
        }

        return_task
    }

fn cancel_song_download(&self, id: ListSongID) {
        let token = {
            let mut downloads = self.active_downloads.lock().unwrap_or_warn();
            let pos = downloads.iter().position(|(song_id, _)| *song_id == id);
            pos.map(|p| downloads.swap_remove(p).1.cancel_token)
        };
        if let Some(token) = token {
            token.cancel();
        }
    }

    pub fn delete_all(&mut self) -> Effects<Self> {
        self.reset()
    }

    pub fn view_browser(&mut self) -> AppCallback {
        AppCallback::ChangeContext(WindowContext::Browser)
    }

    pub fn toggle_shuffle(&mut self) -> Effects<Self> {
        self.shuffle_enabled = !self.shuffle_enabled;

        if self.shuffle_enabled {
            self.shuffle_seed = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            self.generate_shuffle_indices();

            if let (_, Some(playing_idx)) =
                (self.get_cur_playing_id(), self.get_cur_playing_index())
            {
                if let Some(shuffled_pos) =
                    self.shuffle_indices.iter().position(|&i| i == playing_idx)
                {
                    self.cur_selected = shuffled_pos;
                }
            } else {
                self.cur_selected = 0.min(self.get_max_visual_index());
            }
        } else {
            if let Some(playing_idx) = self.get_cur_playing_index() {
                self.cur_selected =
                    playing_idx.min(self.list.get_list_iter().len().saturating_sub(1));
            }
            self.shuffle_indices.clear();
            self.shuffle_visual_map.clear();
        }

        self.cached_title.borrow_mut().take();
        self.regenerate_downloads_debounced()
    }

    pub(super) fn generate_shuffle_indices(&mut self) {
        let len = self.list.get_list_iter().count();
        if len == 0 {
            self.shuffle_indices.clear();
            self.shuffle_visual_map.clear();
            return;
        }

        let mut indices: Vec<usize> = (0..len).collect();

        let mut rng = StdRng::seed_from_u64(self.shuffle_seed);
        for i in (1..len).rev() {
            let j = rng.random_range(0..=i);
            indices.swap(i, j);
        }

        if let Some(current_index) = self.get_cur_playing_index()
            && let Some(pos) = indices.iter().position(|&i| i == current_index)
        {
            indices.swap(0, pos);
        }

        self.shuffle_visual_map = build_visual_map(&indices, len);
        self.shuffle_indices = indices;
    }

    pub fn toggle_search(&mut self) -> Effects<Self> {
        self.search_enabled = !self.search_enabled;
        self.cached_title.borrow_mut().take();

        if self.search_enabled {
            self.pre_search_selected = self.cur_selected;
            self.search_text.clear();
            self.update_search_indices();
        } else {
            let _ = self.clear_search();
            self.cur_selected = self.pre_search_selected.min(self.get_max_visual_index());
        }

        Effects::none()
    }

    pub fn clear_search(&mut self) -> Effects<Self> {
        self.search_text.clear();
        self.cached_title.borrow_mut().take();
        self.update_search_indices();
        self.cur_selected = self.cur_selected.min(self.get_max_visual_index());
        Effects::none()
    }

    pub(super) fn update_search_indices(&mut self) {
        let list_len = self.list.get_list_iter().count();
        let search_lower = self.search_text.to_lowercase();

        if search_lower.is_empty() {
            self.search_indices = (0..list_len).collect();
            self.search_visual_map = build_visual_map(&self.search_indices, list_len);
            return;
        }

        let words: Vec<&str> = search_lower.split_whitespace().collect();

        self.search_indices = self
            .list
            .get_list_iter()
            .enumerate()
            .filter_map(|(actual_idx, song)| {
                if words.iter().all(|word| {
                    song.title_lower.contains(word)
                        || song.album_lower.contains(word)
                        || song.artists_lower.contains(word)
                }) {
                    Some(actual_idx)
                } else {
                    None
                }
            })
            .collect();
        self.search_visual_map = build_visual_map(&self.search_indices, list_len);
    }

    pub(super) fn visual_to_actual_index(&self, visual_index: usize) -> usize {
        let list_len = self.list.get_list_iter().count();
        if list_len == 0 {
            return 0;
        }

        if !self.search_text.is_empty() {
            if self.search_indices.is_empty() {
                return 0;
            }
            let clamped = visual_index.min(self.search_indices.len() - 1);
            return self.search_indices[clamped];
        }

        let base_index = visual_index.min(list_len - 1);
        if self.shuffle_enabled && !self.shuffle_indices.is_empty() {
            self.shuffle_indices[base_index.min(self.shuffle_indices.len() - 1)]
        } else {
            base_index.min(list_len - 1)
        }
    }

    pub(super) fn actual_to_visual_index(&self, actual_index: usize) -> Option<usize> {
        if !self.search_text.is_empty() {
            return self
                .search_visual_map
                .get(actual_index)
                .copied()
                .flatten();
        }

        if self.shuffle_enabled && !self.shuffle_indices.is_empty() {
            return self
                .shuffle_visual_map
                .get(actual_index)
                .copied()
                .flatten();
        }

        Some(actual_index)
    }

    fn get_next_song_id(&self) -> Option<ListSongID> {
        let current_visual = self
            .get_cur_playing_index()
            .and_then(|idx| self.actual_to_visual_index(idx))?;

        for next_visual in (current_visual + 1)..=self.get_max_visual_index() {
            let next_actual = self.visual_to_actual_index(next_visual);
            let Some(next_id) = self.get_id_from_index(next_actual) else {
                continue;
            };
            if !self.is_session_dead_video(next_id) {
                return Some(next_id);
            }
            debug!("skipping session-dead song on auto-advance id={next_id:?}");
        }
        None
    }

    /// First song (in visual order) not remembered as session-dead, or `None`
    /// if every song is dead/absent. Used by the end-of-queue wrap path so a
    /// dead-only queue stops instead of re-playing the refused song.
    fn first_live_song_id(&self) -> Option<ListSongID> {
        for visual in 0..=self.get_max_visual_index() {
            let actual = self.visual_to_actual_index(visual);
            let Some(id) = self.get_id_from_index(actual) else {
                continue;
            };
            if !self.is_session_dead_video(id) {
                return Some(id);
            }
        }
        None
    }

    fn is_session_dead_video(&self, id: ListSongID) -> bool {
        self.get_song_from_id(id)
            .map(|s| self.list.session_dead_videos.contains(s.video_id.get_raw()))
            .unwrap_or(false)
    }

    fn get_prev_song_id(&self) -> Option<ListSongID> {
        let current_visual = self
            .get_cur_playing_index()
            .and_then(|idx| self.actual_to_visual_index(idx))?;

        if current_visual == 0 {
            return None;
        }

        let prev_visual = current_visual.saturating_sub(1);
        let prev_actual = self.visual_to_actual_index(prev_visual);
        self.get_id_from_index(prev_actual)
    }

    pub(super) fn get_max_visual_index(&self) -> usize {
        let count = if !self.search_text.is_empty() {
            self.search_indices.len()
        } else if self.shuffle_enabled {
            self.shuffle_indices.len()
        } else {
            self.list.get_list_iter().count()
        };

        count.saturating_sub(1)
    }

    fn cancel_all_downloads(&mut self) {
        let mut downloads = self.active_downloads.lock().unwrap_or_warn();
        for (_, task) in downloads.iter() {
            task.cancel_token.cancel();
        }
        downloads.clear();
        self.download_queue.clear();
        self.preloaded_sources.clear();
    }

    fn cancel_out_of_scope_downloads(&mut self, scope_ids: &[ListSongID]) {
        let removed_ids: Vec<ListSongID> = {
            let mut downloads = self.active_downloads.lock().unwrap_or_warn();
            let mut removed = Vec::new();
            downloads.retain(|(song_id, task)| {
                if scope_ids.contains(song_id) {
                    true
                } else {
                    task.cancel_token.cancel();
                    removed.push(*song_id);
                    false
                }
            });
            removed
        };
        for removed_id in &removed_ids {
            if let Some(song) = self.get_mut_song_from_id(*removed_id) {
                song.download_status = DownloadStatus::None;
            }
        }
        for id in &removed_ids {
            self.preloaded_sources.remove(id);
            self.download_queue.retain(|qid| qid != id);
        }
        let stale_preloaded: Vec<ListSongID> = self
            .preloaded_sources
            .keys()
            .filter(|id| !scope_ids.contains(id))
            .copied()
            .collect();
        for id in stale_preloaded {
            self.preloaded_sources.remove(&id);
        }
    }

    fn regenerate_downloads_for_current(&mut self) -> Effects<Self> {
        if let Some(current_id) = self.get_cur_playing_id() {
            self.drop_unscoped_from_id(current_id);
            self.download_upcoming_from_id(current_id)
        } else {
            Effects::none()
        }
    }

    /// Regenerate the download scope after a shuffle toggle, coalesced to a
    /// trailing `SHUFFLE_REGEN_DEBOUNCE_MS` window. Holding (or repeating) the
    /// shuffle key toggles many times per second; without the debounce each
    /// toggle spawned a fresh resolve + download before the previous one was
    /// cancelled, so a held key could run several yt-dlp processes at once
    /// (resolve runs outside the download semaphore). The final toggle in a
    /// burst wins: any still-pending effect is cancelled before scheduling the
    /// next one, so at most one regeneration survives the burst. The shuffle
    /// *order* is always applied synchronously in `toggle_shuffle`; only the
    /// network-triggering scope regeneration is delayed.
    fn regenerate_downloads_debounced(&mut self) -> Effects<Self> {
        if self.get_cur_playing_id().is_none() {
            // Nothing is playing, so the eventual regen would no-op anyway.
            // Don't schedule a timer that only wakes to do nothing on every
            // idle shuffle toggle — and clear any stale pending one.
            if let Some(token) = self.shuffle_regen_token.take() {
                token.cancel();
            }
            return Effects::none();
        }
        if let Some(token) = self.shuffle_regen_token.take() {
            token.cancel();
        }
        let token = tokio_util::sync::CancellationToken::new();
        self.shuffle_regen_token = Some(token.clone());

        Effects::new(move |_server: &crate::app::server::ArcServer| async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    Box::new(|_: &mut Playlist| Effects::none())
                        as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(SHUFFLE_REGEN_DEBOUNCE_MS)) => {
                    let fired = token.clone();
                    Box::new(move |this: &mut Playlist| this.apply_fired_shuffle_regen(&fired))
                        as Box<dyn FnOnce(&mut Playlist) -> Effects<Playlist> + Send>
                }
            }
        })
    }

    /// Apply the scope regen for a debounce whose sleep branch won, and clear
    /// the pending-token field — but only if it still holds *this* token. A
    /// newer toggle with a freshly scheduled token (arriving between the sleep
    /// win and this callback) supersedes us: its own debounce owns the regen,
    /// so this callback must neither rebuild the scope nor clear the newer
    /// token — with the field clobbered the newer debounce would lose the
    /// ability to cancel itself. Only the field's live token regenerates.
    pub(super) fn apply_fired_shuffle_regen(
        &mut self,
        fired: &tokio_util::sync::CancellationToken,
    ) -> Effects<Self> {
        if self.shuffle_regen_token.as_ref() != Some(fired) {
            return Effects::none();
        }
        self.shuffle_regen_token = None;
        self.regenerate_downloads_for_current()
    }

    pub fn handle_song_download_progress_update(
        &mut self,
        update: DownloadProgressUpdate,
        id: ListSongID,
    ) -> Effects<Self> {
        let video_id = self
            .get_song_from_id(id)
            .map(|s| s.video_id.get_raw().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        match update {
            DownloadProgressUpdate::Downloading => {
                debug!("download_started: song_id={}", video_id);
                if let Some(idx) = self.get_index_from_id(id)
                    && let Some(song) = self.list.get_list_iter_mut().nth(idx)
                {
                    song.download_status = DownloadStatus::Downloading(Percentage(0));
                }
                Effects::none()
            }
            DownloadProgressUpdate::Completed(decoder) => {
                debug!("download_done: song_id={}", video_id);
                self.consecutive_download_failures = 0;
                if let Some(idx) = self.get_index_from_id(id)
                    && let Some(s) = self.list.get_list_iter_mut().nth(idx)
                {
                    s.download_status = DownloadStatus::Downloaded;
                }
                self.active_downloads
                    .lock()
                    .unwrap_or_warn()
                    .retain(|(song_id, _)| *song_id != id);

                let mut effect = self.handle_song_downloaded(id);

                if let PlayState::Buffering(target_id) = self.play_status
                    && target_id == id
                {
                    let task = Effects::new_stream(
                        move |server: &crate::app::server::ArcServer| {
                            playback_stream(Arc::clone(server), id, decoder)
                        }
                    );
                    effect = effect.push(task);
                } else {
                    self.preloaded_sources.insert(id, decoder);
                }

                if let Some(next_id) = self.download_queue.pop_front() {
                    debug!("queue_starting_next: song_id={:?}", next_id);
                    effect = effect.push(self.download_song(next_id));
                }
                effect
            }
            DownloadProgressUpdate::Error(e) => {
                if is_cancellation_error(&e) {
                    debug!("download_error: song_id={}, error={}", video_id, e);
                } else {
                    warn!("download_error: song_id={}, error={}", video_id, e);
                }
                if let Some(idx) = self.get_index_from_id(id)
                    && let Some(song) = self.list.get_list_iter_mut().nth(idx)
                {
                    song.download_status = DownloadStatus::Failed;
                }
                self.active_downloads
                    .lock()
                    .unwrap_or_warn()
                    .retain(|(song_id, _)| *song_id != id);

                let mut effect = Effects::none();
                if matches!(self.play_status, PlayState::Buffering(target) if target == id) {
                    if is_cancellation_error(&e) {
                        debug!("download failed while buffering, skipping: {}", e);
                    } else {
                        warn!("download failed while buffering, skipping: {}", e);
                        let is_dead = is_dead_video_error(&e);
                        let is_auth = is_auth_error(&e);
                        if is_auth {
                            self.notify_auth_error(&e);
                        }
                        if is_dead {
                            let (video_id, title) = self
                                .get_song_from_id(id)
                                .map(|s| (s.video_id.get_raw().to_string(), s.title.clone()))
                                .unwrap_or_default();
                            self.list.session_dead_videos.insert(video_id);
                            if self.notifications_enabled && !title.is_empty() {
                                let body = format!(
                                    "{title} — no longer available on YouTube, skipped"
                                );
                                spawn_notification("Song Unavailable", &body, 5000);
                            }
                        }
                        // A dead video or auth failure is a definitive per-song /
                        // per-session condition, never a sign of a systemic
                        // download problem. Only transient/systemic failures
                        // (spawn errors, rate limits, format loss) count toward
                        // the halt that protects the queue from draining.
                        if !is_dead && !is_auth {
                            self.consecutive_download_failures =
                                self.consecutive_download_failures.saturating_add(1);
                            if self.consecutive_download_failures
                                >= HALT_AFTER_CONSECUTIVE_FAILURES
                            {
                                self.halt_on_download_failures(&e);
                                return Effects::none();
                            }
                        }
                    }
                    effect = effect.push(self.handle_set_to_error(id));
                }
                effect
            }
        }
    }

    pub fn handle_volume_update(&mut self, response: VolumeUpdate) {
        self.volume = response.0
    }

    pub fn handle_set_song_play_progress(
        &mut self,
        d: Duration,
        id: ListSongID,
    ) -> Effects<Self> {
        if !self.check_id_is_cur(id) {
            return Effects::none();
        }

        if d.is_zero() {
            debug!("play_progress: zero duration received for id={:?}", id);
        }
        self.cur_played_dur = Some(d);

        if let Some(duration_dif) = {
            let cur_dur = self
                .get_cur_playing_song()
                .and_then(|song| song.actual_duration);
            self.cur_played_dur
                .as_ref()
                .zip(cur_dur)
                .map(|(d1, d2)| d2.saturating_sub(*d1))
        } && duration_dif
            .saturating_sub(GAPLESS_PLAYBACK_THRESHOLD)
            .is_zero()
            && !matches!(self.queue_status, QueueState::Queued(_))
            && let Some(next_song) = self.get_next_song()
            && !matches!(
                &next_song.download_status,
                DownloadStatus::Downloaded | DownloadStatus::Failed
            )
        {
            let next_id = next_song.id;
            debug!("Queuing up song!");
            let effect = self.download_song(next_id);
            self.queue_status = QueueState::Queued(next_id);
            return effect;
        }

        Effects::none()
    }

    pub fn handle_done_playing(&mut self, id: ListSongID) -> Effects<Self> {
        if !self.check_id_is_cur(id) && self.queue_status != QueueState::Queued(id) {
            return Effects::none();
        }

        if self.queue_status == QueueState::Queued(id) {
            self.queue_status = QueueState::NotQueued;
            return Effects::none();
        }

        self.autoplay_next_or_stop(id)
    }

    pub fn handle_playing(
        &mut self,
        duration: Option<Duration>,
        id: ListSongID,
    ) -> Effects<Self> {
        if let Some(song) = self.get_mut_song_from_id(id) {
            song.actual_duration = duration;
        }

        match self.play_status {
            PlayState::Paused(p_id) if p_id == id => {
                self.play_status = PlayState::Playing(id);
            }
            PlayState::Buffering(b_id) if b_id == id => {
                self.play_status = PlayState::Playing(id);
            }
            _ => {}
        }
        // Calling regenerate_downloads_for_current immediately below applies
        // the current shuffle order right now. Cancel any still-pending
        // debounced shuffle regen: running it again ~100ms later would just
        // re-derive the same scope with no effect.
        if let Some(token) = self.shuffle_regen_token.take() {
            token.cancel();
        }
        self.regenerate_downloads_for_current()
    }

    pub fn handle_set_to_error(&mut self, id: ListSongID) -> Effects<Self> {
        debug!("Received message that song had a playback error {:?}", id);
        if self.check_id_is_cur(id) {
            debug!("Setting song state to Error {:?}", id);
            self.play_status = PlayState::Error(id);
            debug!("Skipping to next song after error");
            self.play_next_or_stop(id)
        } else {
            Effects::none()
        }
    }

    pub fn handle_stopped(&mut self, id: Stopped<ListSongID>) {
        let Stopped(id) = id;
        debug!("Received message that playback {:?} has been stopped", id);
        if self.check_id_is_cur(id) {
            debug!("Stopping {:?}", id);
            self.play_status = PlayState::NotPlaying;
            self.preloaded_sources.clear();
            cache_clear();
        }
    }

    pub fn handle_all_stopped(&mut self, _: AllStopped) {
        if matches!(self.play_status, PlayState::NotPlaying) {
            self.preloaded_sources.clear();
            cache_clear();
        }
    }
}

/// Fire-and-forget desktop notification. Only runs when called from inside a
/// Tokio runtime (the app event loop); callers guard `notifications_enabled`
/// themselves. Single source of truth for the notify_rust invocation.
fn spawn_notification(summary: &str, body: &str, timeout_ms: u32) {
    if let Ok(rt) = tokio::runtime::Handle::try_current() {
        let summary = summary.to_string();
        let body = body.to_string();
        drop(rt.spawn(async move {
            if let Err(e) = Notification::new()
                .summary(&summary)
                .body(&body)
                .appname("youtui")
                .timeout(Timeout::Milliseconds(timeout_ms))
                .show()
            {
                debug!("notification failed: {e}");
            }
        }));
    }
}
