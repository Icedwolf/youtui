# Youtui Optimization Roadmap

**Last Updated**: 2026-06-05
**Status**: Core optimizations active, performance regression fixed

---

## ✅ COMPLETED OPTIMIZATIONS

### Network Performance
- [x] **Connection pooling** - 8 idle connections, 90s timeout, TCP keepalive
- [x] **Connection timeouts** - 15s connect, 30s total timeout

### Download Performance
- [x] **Dynamic concurrency** - Adapts to network speed (fast=4, normal=3, slow=1)
- [x] **Streaming** - Already streams via yt-dlp stdout

### Memory Optimization
- [x] **Cached artists_string / track_no_string** - Pre-computed at song creation, returned via `Cow::Borrowed` in `get_field()`. **Do NOT remove** — was removed once causing per-frame allocation regression, then restored.
- [x] **Lowercased search fields** - `title_lower`, `album_lower`, `artists_lower` cached for O(1) case-insensitive search

### Caching
- [x] **Thumbnail LRU cache** — Removed (the `song_thumbnail_downloader.rs` and `lru` crate were deleted). Notifications now skip remote thumbnails entirely for instant responsiveness — only `file://` URLs are used.
- [x] **Metadata caching** - Already uses in-memory API reuse

### Code Quality
- [x] **Error handling** - Uses anyhow consistently
- [x] **Clean build** - 0 errors, 1 warning (pre-existing dead code)

---

## 🚧 IN PROGRESS

### Test Infrastructure (from 2026-06-05 session)
- [ ] **Criterion benchmarks** for `get_field` hot-path (perf regression detection)
- [ ] **`TestBackend` render snapshot tests** for playlist + browser views
- [ ] **`profile-render` feature flag** with per-draw timing (warn if >8ms)
- [ ] **State-model integration tests** (keypress → expected state)

---

## 📋 REMAINING ITEMS (Optional/Low Priority)

### Architecture
- [ ] Replace async-callback-manager with native Tokio (29 usages, ~2000 line rewrite)
- [ ] Simplify component hierarchy (deep nesting)

### Future Enhancements
- [ ] Stats Tab in UI (new WindowContext)
- [ ] Extended metrics (CPU, memory, cache hit rates)

---

## 🎯 Performance Targets

| Metric | Target | Status |
|--------|--------|--------|
| Per-frame allocation in `get_field` | Zero (Cow::Borrowed) | ✅ Cached fields restored |
| Network connections | Pooled | ✅ 8 idle, 90s timeout |
| Search responsiveness | O(1) per char | ✅ Pre-lowercased fields |
| Notification latency | Instant (<100ms) | ✅ No thumbnail download in path |

---

## 🔧 Technical Debt

| Area | Severity | Status |
|------|----------|--------|
| Async Architecture | High | ✅ Working, complex to refactor |
| Memory Management | Medium | ✅ Cached hot-path fields |
| Testing Infrastructure | Medium | 🔜 Criterion, snapshots, state tests planned |
| Error Handling | Medium | ✅ Clean |
