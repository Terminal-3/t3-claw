//! SSE connection manager for broadcasting events to browser tabs.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures::Stream;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use uuid::Uuid;

use crate::channels::web::types::AppEvent;

/// Maximum number of concurrent SSE/WebSocket connections.
/// Prevents resource exhaustion from connection flooding.
pub const DEFAULT_MAX_CONNECTIONS: u64 = 100;

/// Per-user replay buffer cap. A typical chat turn emits roughly 5-20
/// events (Thinking, ToolCall, ToolResult, Response, Done); N=50 covers
/// the most recent 2-3 turns, which is plenty for the SSE-reconnect gap
/// (followup #33 option 2) without bloating memory. Global / unscoped
/// events (e.g. Heartbeat) are not buffered — they carry no per-turn
/// state and don't need replay.
const REPLAY_BUFFER_CAP: usize = 50;

/// Envelope for broadcast events: carries an optional user scope.
///
/// `user_id = None` means the event is global (e.g. Heartbeat) and delivered
/// to all subscribers. `user_id = Some(id)` means the event is only delivered
/// to subscribers that match that user_id.
#[derive(Debug, Clone)]
pub(crate) struct ScopedEvent {
    pub(crate) id: String,
    pub(crate) user_id: Option<String>,
    pub(crate) event: AppEvent,
}

/// Manages SSE broadcast to all connected browser tabs.
///
/// In multi-user mode, events are scoped by user_id so that each subscriber
/// only receives events intended for their user (plus global events like
/// Heartbeat). In single-user mode, all events are delivered to all subscribers
/// (backwards compatible).
pub struct SseManager {
    tx: broadcast::Sender<ScopedEvent>,
    connection_count: Arc<AtomicU64>,
    boot_id: Arc<str>,
    next_event_id: Arc<AtomicU64>,
    max_connections: u64,
    /// Per-user ring of recent user-scoped events, replayed on subscribe
    /// to close the SSE-reconnect gap. Keyed by user_id; capped at
    /// `REPLAY_BUFFER_CAP` per user. In-memory only — dropped on manager
    /// drop / process restart.
    replay: Arc<Mutex<HashMap<String, VecDeque<ScopedEvent>>>>,
}

impl SseManager {
    /// Create a new SSE manager.
    pub fn new() -> Self {
        Self::with_max_connections(DEFAULT_MAX_CONNECTIONS)
    }

    /// Create a new SSE manager with a custom connection limit.
    pub fn with_max_connections(max_connections: u64) -> Self {
        // Buffer 256 events; slow clients will miss events (acceptable for SSE with reconnect)
        let (tx, _) = broadcast::channel(256);
        Self {
            tx,
            connection_count: Arc::new(AtomicU64::new(0)),
            boot_id: Arc::<str>::from(Uuid::new_v4().to_string()),
            next_event_id: Arc::new(AtomicU64::new(1)),
            max_connections,
            replay: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create an SSE manager that reuses an existing broadcast sender.
    ///
    /// This preserves the broadcast channel across `rebuild_state` calls so
    /// that sender handles captured by other components remain valid.
    ///
    /// **Important:** The connection counter is reset to zero and a fresh
    /// `boot_id` is generated (resetting the event-ID sequence). This method
    /// must only be called before the server starts accepting connections
    /// (i.e., during startup wiring). Calling it after connections are
    /// established will break connection tracking, allow exceeding
    /// `max_connections`, and invalidate event-ID dedup for connected clients.
    pub(crate) fn from_sender(tx: broadcast::Sender<ScopedEvent>, max_connections: u64) -> Self {
        Self {
            tx,
            connection_count: Arc::new(AtomicU64::new(0)),
            boot_id: Arc::<str>::from(Uuid::new_v4().to_string()),
            next_event_id: Arc::new(AtomicU64::new(1)),
            max_connections,
            replay: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get a clone of the broadcast sender for use by other components.
    pub(crate) fn sender(&self) -> broadcast::Sender<ScopedEvent> {
        self.tx.clone()
    }

    /// Get the configured connection limit.
    pub fn max_connections(&self) -> u64 {
        self.max_connections
    }

    fn next_scoped_event(&self, user_id: Option<String>, event: AppEvent) -> ScopedEvent {
        let seq = self.next_event_id.fetch_add(1, Ordering::Relaxed);
        ScopedEvent {
            id: format!("{}:{seq}", self.boot_id),
            user_id,
            event,
        }
    }

    /// Broadcast an event to all connected clients (global/unscoped).
    pub fn broadcast(&self, event: AppEvent) {
        let _ = self.tx.send(self.next_scoped_event(None, event));
    }

    /// Broadcast an event scoped to a specific user.
    ///
    /// Only subscribers for this user_id (or unscoped subscribers) will
    /// receive the event. The event is also appended to the per-user
    /// replay buffer so a client reconnecting within the window can
    /// catch up (followup #33 option 2).
    pub fn broadcast_for_user(&self, user_id: &str, event: AppEvent) {
        let scoped = self.next_scoped_event(Some(user_id.to_string()), event);
        self.append_replay(user_id, scoped.clone());
        let _ = self.tx.send(scoped);
    }

    fn append_replay(&self, user_id: &str, scoped: ScopedEvent) {
        let mut guard = match self.replay.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let buf = guard.entry(user_id.to_string()).or_default();
        buf.push_back(scoped);
        while buf.len() > REPLAY_BUFFER_CAP {
            buf.pop_front();
        }
    }

    /// Snapshot the current replay buffer for a user, filtered by
    /// `last_event_id` so we don't redeliver events the client already
    /// saw. The returned Vec is in insertion order (oldest first).
    fn snapshot_replay(
        &self,
        user_id: &str,
        last_event_id: Option<&str>,
    ) -> Vec<ScopedEvent> {
        let guard = match self.replay.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(buf) = guard.get(user_id) else {
            return Vec::new();
        };
        buf.iter()
            .filter(|ev| is_event_after(last_event_id, &ev.id))
            .cloned()
            .collect()
    }

    /// Get current number of active connections.
    pub fn connection_count(&self) -> u64 {
        self.connection_count.load(Ordering::Relaxed)
    }

    /// Create a raw broadcast subscription for non-SSE consumers (e.g. WebSocket).
    ///
    /// When `user_id` is `Some`, only events scoped to that user (or global
    /// events) are delivered. When `None`, all events are delivered (single-user
    /// backwards compatibility).
    ///
    /// Returns `None` if the maximum connection limit has been reached.
    pub fn subscribe_raw(
        &self,
        user_id: Option<String>,
    ) -> Option<impl Stream<Item = AppEvent> + Send + 'static + use<>> {
        // Atomically increment only if below the limit. This prevents
        // concurrent callers from overshooting max_connections.
        let counter = Arc::clone(&self.connection_count);
        let max = self.max_connections;
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current < max {
                    Some(current + 1)
                } else {
                    None
                }
            })
            .ok()?;
        // Subscribe to the live channel BEFORE snapshotting the replay
        // buffer so no event can slip through the gap; the live stream
        // is then filtered to skip any id we already replayed.
        let rx = self.tx.subscribe();
        let (replay_events, replay_max_seq) = match &user_id {
            Some(uid) => {
                let snapshot = self.snapshot_replay(uid, None);
                let max_seq = snapshot
                    .iter()
                    .filter_map(|ev| parse_event_id(&ev.id).map(|(_, seq)| seq))
                    .max();
                (snapshot, max_seq)
            }
            None => (Vec::new(), None),
        };

        let user_id_for_live = user_id.clone();
        let live = BroadcastStream::new(rx).filter_map(move |result| match result {
            Ok(scoped) => {
                // Drop any event that's already covered by the replay
                // snapshot (same boot_id, seq <= max replayed seq).
                if let Some(max_seq) = replay_max_seq
                    && let Some((_, seq)) = parse_event_id(&scoped.id)
                    && seq <= max_seq
                {
                    return None;
                }
                // Global events (user_id=None) always pass through.
                // Scoped events only pass if the subscriber matches (or subscriber is unscoped).
                match (&user_id_for_live, &scoped.user_id) {
                    (_, None) => Some(scoped.event), // global -> all
                    (None, _) => Some(scoped.event), // unscoped subscriber -> all
                    (Some(sub), Some(ev)) if sub == ev => Some(scoped.event), // match
                    _ => None,                       // different user -> skip
                }
            }
            Err(_) => None,
        });

        let replay_stream = tokio_stream::iter(replay_events.into_iter().map(|s| s.event));
        let stream = replay_stream.chain(live);

        Some(CountedStream {
            inner: stream,
            counter,
        })
    }

    /// Create a new SSE stream for a client connection.
    ///
    /// When `user_id` is `Some`, only events for that user (or global events)
    /// are delivered. When `None`, all events are delivered.
    ///
    /// Returns `None` if the maximum connection limit has been reached.
    pub fn subscribe(
        &self,
        user_id: Option<String>,
        last_event_id: Option<String>,
    ) -> Option<Sse<impl Stream<Item = Result<Event, Infallible>> + Send + 'static + use<>>> {
        // Atomically increment only if below the limit.
        let counter = Arc::clone(&self.connection_count);
        let max = self.max_connections;
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                if current < max {
                    Some(current + 1)
                } else {
                    None
                }
            })
            .ok()?;
        // Subscribe BEFORE snapshotting the replay buffer so no event
        // can slip through the gap; the live stream then filters out
        // any id <= the highest replayed seq.
        let rx = self.tx.subscribe();
        let (replay_events, replay_max_seq) = match &user_id {
            Some(uid) => {
                let snapshot = self.snapshot_replay(uid, last_event_id.as_deref());
                let max_seq = snapshot
                    .iter()
                    .filter_map(|ev| parse_event_id(&ev.id).map(|(_, seq)| seq))
                    .max();
                (snapshot, max_seq)
            }
            None => (Vec::new(), None),
        };

        let user_id_for_live = user_id.clone();
        let last_event_id_for_live = last_event_id.clone();
        let live = BroadcastStream::new(rx)
            .filter_map(move |result| match result {
                Ok(scoped) => {
                    if let Some(max_seq) = replay_max_seq
                        && let Some((_, seq)) = parse_event_id(&scoped.id)
                        && seq <= max_seq
                    {
                        return None;
                    }
                    match (&user_id_for_live, &scoped.user_id) {
                        (_, None) => Some(scoped),
                        (None, _) => Some(scoped),
                        (Some(sub), Some(ev)) if sub == ev => Some(scoped),
                        _ => None,
                    }
                }
                Err(_) => None,
            })
            .filter_map(move |scoped| {
                if !is_event_after(last_event_id_for_live.as_deref(), &scoped.id) {
                    return None;
                }
                scoped_to_sse_event(scoped)
            });

        let replay_stream =
            tokio_stream::iter(replay_events.into_iter().filter_map(scoped_to_sse_event));
        let stream = replay_stream.chain(live);

        // Wrap in a stream that decrements on drop
        let counted_stream = CountedStream {
            inner: stream,
            counter,
        };

        Some(
            Sse::new(counted_stream)
                .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)).text("")),
        )
    }
}

fn parse_event_id(id: &str) -> Option<(&str, u64)> {
    let (boot_id, seq) = id.split_once(':')?;
    Some((boot_id, seq.parse().ok()?))
}

fn scoped_to_sse_event(scoped: ScopedEvent) -> Option<Result<Event, Infallible>> {
    let data = match serde_json::to_string(&scoped.event) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("Failed to serialize SSE event: {}", e);
            return None;
        }
    };
    let event_type = scoped.event.event_type();
    Some(Ok(Event::default()
        .id(scoped.id)
        .event(event_type)
        .data(data)))
}

fn is_event_after(last_event_id: Option<&str>, current_event_id: &str) -> bool {
    let Some(last_event_id) = last_event_id else {
        return true;
    };
    let Some((last_boot_id, last_seq)) = parse_event_id(last_event_id) else {
        return true;
    };
    let Some((current_boot_id, current_seq)) = parse_event_id(current_event_id) else {
        return true;
    };
    if last_boot_id != current_boot_id {
        return true;
    }
    current_seq > last_seq
}

impl Default for SseManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Stream wrapper that decrements connection count on drop.
///
/// When the SSE client disconnects, this stream is dropped
/// and the counter is decremented.
struct CountedStream<S> {
    inner: S,
    counter: Arc<AtomicU64>,
}

impl<S: Stream + Unpin> Stream for CountedStream<S> {
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for CountedStream<S> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sse_manager_creation() {
        let manager = SseManager::new();
        assert_eq!(manager.connection_count(), 0);
        assert_eq!(manager.max_connections(), DEFAULT_MAX_CONNECTIONS);
    }

    #[test]
    fn test_broadcast_without_receivers() {
        let manager = SseManager::new();
        // Should not panic even with no receivers
        manager.broadcast(AppEvent::Heartbeat);
    }

    #[tokio::test]
    async fn test_broadcast_to_receiver() {
        let manager = SseManager::new();
        let mut stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));

        manager.broadcast(AppEvent::Status {
            message: "test".to_string(),
            thread_id: None,
        });

        let event = stream.next().await.unwrap();
        match event {
            AppEvent::Status { message, .. } => assert_eq!(message, "test"),
            _ => panic!("unexpected event type"),
        }
    }

    #[tokio::test]
    async fn test_subscribe_raw_receives_events() {
        let manager = SseManager::new();
        let mut stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));

        assert_eq!(manager.connection_count(), 1);

        manager.broadcast(AppEvent::Thinking {
            message: "working".to_string(),
            thread_id: None,
        });

        let event = stream.next().await.unwrap();
        match event {
            AppEvent::Thinking { message, .. } => assert_eq!(message, "working"),
            _ => panic!("Expected Thinking event"),
        }
    }

    #[tokio::test]
    async fn test_subscribe_raw_decrements_on_drop() {
        let manager = SseManager::new();
        {
            let _stream = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));
            assert_eq!(manager.connection_count(), 1);
        }
        // Stream dropped, counter should decrement
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_raw_multiple_subscribers() {
        let manager = SseManager::new();
        let mut s1 = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));
        let mut s2 = Box::pin(manager.subscribe_raw(None).expect("should subscribe"));
        assert_eq!(manager.connection_count(), 2);

        manager.broadcast(AppEvent::Heartbeat);

        let e1 = s1.next().await.unwrap();
        let e2 = s2.next().await.unwrap();
        assert!(matches!(e1, AppEvent::Heartbeat));
        assert!(matches!(e2, AppEvent::Heartbeat));

        drop(s1);
        assert_eq!(manager.connection_count(), 1);
        drop(s2);
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_raw_rejects_over_limit() {
        let mut manager = SseManager::new();
        manager.max_connections = 2; // Low limit for testing

        let _s1 = Box::pin(manager.subscribe_raw(None).expect("first should succeed"));
        let _s2 = Box::pin(manager.subscribe_raw(None).expect("second should succeed"));
        assert_eq!(manager.connection_count(), 2);

        // Third should be rejected
        assert!(manager.subscribe_raw(None).is_none());
        assert!(manager.subscribe(None, None).is_none());
    }

    #[tokio::test]
    async fn test_scoped_events_filtered_by_user() {
        let manager = SseManager::new();
        let mut alice = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );
        let mut bob = Box::pin(
            manager
                .subscribe_raw(Some("bob".to_string()))
                .expect("subscribe"),
        );

        // Send event scoped to alice
        manager.broadcast_for_user(
            "alice",
            AppEvent::Status {
                message: "alice only".to_string(),
                thread_id: None,
            },
        );

        // Send global event
        manager.broadcast(AppEvent::Heartbeat);

        // Alice gets her scoped event
        let e = alice.next().await.unwrap();
        assert!(matches!(e, AppEvent::Status { .. }));

        // Alice also gets the global heartbeat
        let e = alice.next().await.unwrap();
        assert!(matches!(e, AppEvent::Heartbeat));

        // Bob only gets the global heartbeat (alice's event was filtered)
        let e = bob.next().await.unwrap(); // safety: test-only
        assert!(matches!(e, AppEvent::Heartbeat)); // safety: test assertion
    }

    #[test]
    fn test_is_event_after_filters_same_boot_duplicates() {
        assert!(is_event_after(Some("boot:4"), "boot:5"));
        assert!(!is_event_after(Some("boot:5"), "boot:5"));
        assert!(!is_event_after(Some("boot:6"), "boot:5"));
    }

    #[test]
    fn test_is_event_after_ignores_other_boots_and_invalid_ids() {
        assert!(is_event_after(Some("old-boot:99"), "new-boot:1"));
        assert!(is_event_after(Some("not-an-id"), "new-boot:1"));
        assert!(is_event_after(Some("boot:1"), "also-bad"));
    }

    /// Make a Status event with a recognisable message; lets us assert
    /// replay ordering by reading the message back.
    fn status(msg: &str) -> AppEvent {
        AppEvent::Status {
            message: msg.to_string(),
            thread_id: None,
        }
    }

    #[tokio::test]
    async fn test_replay_yields_buffered_events_before_live() {
        let manager = SseManager::new();

        // Pre-populate the buffer BEFORE any subscriber exists.
        manager.broadcast_for_user("alice", status("buffered-1"));
        manager.broadcast_for_user("alice", status("buffered-2"));

        // Now subscribe — the snapshot should replay both events.
        let mut alice = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );

        // A new live event arrives after subscribe.
        manager.broadcast_for_user("alice", status("live-1"));

        let mut seen = Vec::new();
        for _ in 0..3 {
            match alice.next().await.expect("event") {
                AppEvent::Status { message, .. } => seen.push(message),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(seen, vec!["buffered-1", "buffered-2", "live-1"]);
    }

    #[tokio::test]
    async fn test_replay_buffer_caps_at_replay_buffer_cap() {
        let manager = SseManager::new();
        // Push more than the cap; oldest should be dropped.
        let total = REPLAY_BUFFER_CAP + 5;
        for i in 0..total {
            manager.broadcast_for_user("alice", status(&format!("evt-{i}")));
        }

        let mut alice = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );

        // First replayed event must be evt-5 (we dropped 0..5).
        let first = alice.next().await.expect("event");
        match first {
            AppEvent::Status { message, .. } => {
                let first_idx = total - REPLAY_BUFFER_CAP; // = 5
                assert_eq!(message, format!("evt-{first_idx}"));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        // Drain the remaining buffered events; should be REPLAY_BUFFER_CAP - 1.
        let mut count = 1;
        for _ in 1..REPLAY_BUFFER_CAP {
            let _ = alice.next().await.expect("event");
            count += 1;
        }
        assert_eq!(count, REPLAY_BUFFER_CAP);
    }

    #[tokio::test]
    async fn test_replay_buffers_are_per_user() {
        let manager = SseManager::new();
        manager.broadcast_for_user("alice", status("alice-1"));
        manager.broadcast_for_user("bob", status("bob-1"));
        manager.broadcast_for_user("alice", status("alice-2"));

        // Bob subscribes — should only see his own buffered events.
        let mut bob = Box::pin(
            manager
                .subscribe_raw(Some("bob".to_string()))
                .expect("subscribe"),
        );
        match bob.next().await.expect("event") {
            AppEvent::Status { message, .. } => assert_eq!(message, "bob-1"),
            other => panic!("unexpected event: {other:?}"),
        }

        // Alice subscribes — should only see her own buffered events.
        let mut alice = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );
        match alice.next().await.expect("event") {
            AppEvent::Status { message, .. } => assert_eq!(message, "alice-1"),
            other => panic!("unexpected event: {other:?}"),
        }
        match alice.next().await.expect("event") {
            AppEvent::Status { message, .. } => assert_eq!(message, "alice-2"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_replay_buffer_does_not_survive_manager_drop() {
        // Replay is in-memory only; dropping the manager wipes the
        // buffer (acceptable for option 2 — see the followup #33 note).
        let first = SseManager::new();
        first.broadcast_for_user("alice", status("from-first"));
        drop(first);

        let second = SseManager::new();
        let mut alice = Box::pin(
            second
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );

        // Should NOT see "from-first"; only a live event from the new manager.
        second.broadcast_for_user("alice", status("from-second"));
        match alice.next().await.expect("event") {
            AppEvent::Status { message, .. } => assert_eq!(message, "from-second"),
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_replay_does_not_duplicate_live_events() {
        // Edge case: an event broadcast AFTER the subscriber's snapshot
        // but BEFORE the next poll should appear exactly once, not twice.
        let manager = SseManager::new();
        manager.broadcast_for_user("alice", status("pre-1"));

        let mut alice = Box::pin(
            manager
                .subscribe_raw(Some("alice".to_string()))
                .expect("subscribe"),
        );

        // After subscribe, before polling, push another event.
        manager.broadcast_for_user("alice", status("post-1"));

        // Drain everything available.
        let mut seen = Vec::new();
        for _ in 0..2 {
            match alice.next().await.expect("event") {
                AppEvent::Status { message, .. } => seen.push(message),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(seen, vec!["pre-1", "post-1"]);
    }
}
