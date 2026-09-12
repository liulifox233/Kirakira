//! Headless tests for the decoded-PCM tap. Everything here is driven by a
//! synthetic sample source, so the cursor and ring semantics are verified
//! without an audio device and without kira's render thread.

use super::*;

const ID: AudioInstanceId = AudioInstanceId(1);

fn spec(channels: u32) -> PcmAudioSpec {
    PcmAudioSpec {
        sample_rate: 48_000,
        channels,
    }
}

fn registered(capacity_frames: u32) -> (PcmTap, PcmTapFeed) {
    let tap = PcmTap::new(capacity_frames);
    let feed = tap.register(ID, spec(2));
    (tap, feed)
}

/// Stereo frames where frame `index` is `(index, index + 1000)`.
fn stereo(range: std::ops::Range<usize>) -> Vec<f32> {
    range
        .flat_map(|index| [index as f32, index as f32 + 1000.0])
        .collect()
}

fn decoded(count: usize) -> Arc<[Frame]> {
    (0..count)
        .map(|index| Frame::new(index as f32, -(index as f32)))
        .collect::<Vec<_>>()
        .into()
}

fn instance_of(tap: &PcmTap) -> Arc<TapInstance> {
    tap.lock_instances()
        .get(&ID)
        .cloned()
        .expect("registered instance")
}

#[test]
fn ring_stays_inert_until_the_first_read_arms_it() {
    let (tap, feed) = registered(8);
    feed.push(&stereo(0..4));
    assert!(
        !instance_of(&tap).ring_allocated(),
        "an unread tap must not allocate its ring"
    );

    let snapshot = tap.read(ID, PcmTapWindow::ahead(4)).expect("snapshot");
    assert_eq!(snapshot.available_frames, 0);
    assert!(snapshot.frames.iter().all(|sample| *sample == 0.0));
    assert!(instance_of(&tap).ring_allocated());

    // Coordinates start at the first push after arming.
    feed.push(&stereo(10..14));
    let snapshot = tap.read(ID, PcmTapWindow::ahead(4)).expect("snapshot");
    assert_eq!(snapshot.available_frames, 4);
    assert_eq!(snapshot.frames, stereo(10..14));
}

#[test]
fn window_contents_follow_the_rendered_cursor() {
    let (tap, feed) = registered(16);
    let _ = tap.read(ID, PcmTapWindow::ahead(0));
    feed.push(&stereo(0..16));
    feed.advance_rendered(6);

    let snapshot = tap
        .read(
            ID,
            PcmTapWindow {
                back_frames: 3,
                ahead_frames: 2,
            },
        )
        .expect("snapshot");
    assert_eq!(snapshot.cursor, 6);
    assert_eq!(snapshot.first_frame, 3);
    assert_eq!(snapshot.available_frames, 5);
    assert_eq!(snapshot.frames, stereo(3..8));
}

#[test]
fn ring_keeps_only_the_bounded_window() {
    let (tap, feed) = registered(4);
    let _ = tap.read(ID, PcmTapWindow::ahead(0));
    feed.push(&stereo(0..10));
    feed.advance_rendered(8);

    // Coordinates 6..8 are the oldest frames the 4-frame ring still holds;
    // 1..5 were overwritten and read as silence.
    let snapshot = tap
        .read(
            ID,
            PcmTapWindow {
                back_frames: 7,
                ahead_frames: 0,
            },
        )
        .expect("snapshot");
    assert_eq!(snapshot.first_frame, 1);
    assert_eq!(snapshot.available_frames, 2);
    assert!(snapshot.frames[..5 * 2].iter().all(|sample| *sample == 0.0));
    assert_eq!(snapshot.frames[5 * 2..], stereo(6..8));
}

#[test]
fn cursor_continues_across_a_loop_wrap() {
    let (tap, feed) = registered(32);
    feed.attach_decoded(decoded(100), true);
    feed.set_source_position(40);
    assert_eq!(tap.cursor(ID), Some(40));

    // The stream restarted at source 5; the rendered-frame clock kept going.
    feed.set_source_position(5);
    assert_eq!(tap.cursor(ID), Some(105));

    let snapshot = tap
        .read(
            ID,
            PcmTapWindow {
                back_frames: 2,
                ahead_frames: 1,
            },
        )
        .expect("snapshot");
    assert_eq!(snapshot.source_frame, Some(5));
    assert_eq!(snapshot.first_frame, 103);
    assert_eq!(snapshot.frames, [3.0, -3.0, 4.0, -4.0, 5.0, -5.0]);
    assert_eq!(snapshot.available_frames, 3);

    // The cursor keeps counting past the wrap point.
    feed.set_source_position(6);
    assert_eq!(tap.cursor(ID), Some(106));

    // Reading further back crosses the wrap into the previous iteration.
    let snapshot = tap.read(ID, PcmTapWindow::recent(8)).expect("snapshot");
    assert_eq!(snapshot.first_frame, 98);
    assert_eq!(
        snapshot.frames,
        [
            98.0, -98.0, 99.0, -99.0, 0.0, 0.0, 1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0, 5.0,
            -5.0
        ]
    );
    assert_eq!(snapshot.available_frames, 8);
}

#[test]
fn cursor_freezes_while_paused() {
    let (tap, feed) = registered(32);
    feed.attach_decoded(decoded(64), false);
    feed.set_source_position(10);

    feed.set_paused(true);
    assert_eq!(tap.state(ID), Some(PcmTapState::Paused));
    feed.advance_rendered(5);
    feed.set_source_position(20);
    assert_eq!(tap.cursor(ID), Some(10), "a paused cursor must not move");

    feed.set_paused(false);
    feed.set_source_position(21);
    assert_eq!(
        tap.cursor(ID),
        Some(11),
        "resuming continues from the frozen cursor"
    );
}

#[test]
fn stop_zeroes_the_readable_window() {
    let (tap, feed) = registered(16);
    let _ = tap.read(ID, PcmTapWindow::ahead(0));
    feed.push(&stereo(0..8));
    feed.advance_rendered(4);
    assert_eq!(
        tap.read_recent(ID, 4).expect("snapshot").available_frames,
        4
    );

    feed.stop();
    let snapshot = tap.read_recent(ID, 4).expect("snapshot");
    assert_eq!(snapshot.state, PcmTapState::Stopped);
    assert_eq!(snapshot.available_frames, 0);
    assert_eq!(snapshot.frames, vec![0.0; 8]);

    // Nothing after a stop reaches the tap again.
    feed.push(&stereo(20..24));
    feed.set_source_position(9);
    feed.advance_rendered(9);
    assert_eq!(tap.cursor(ID), Some(4));
    assert_eq!(
        tap.read_recent(ID, 4).expect("snapshot").available_frames,
        0
    );
}

#[test]
fn seek_moves_the_cursor_with_the_stream() {
    let (tap, feed) = registered(32);
    feed.attach_decoded(decoded(200), false);
    feed.set_source_position(50);

    feed.notify_seek(120);
    assert_eq!(tap.cursor(ID), Some(120));
    let snapshot = tap.read(ID, PcmTapWindow::ahead(2)).expect("snapshot");
    assert_eq!(snapshot.source_frame, Some(120));
    assert_eq!(snapshot.frames, [120.0, -120.0, 121.0, -121.0]);

    feed.notify_seek(10);
    assert_eq!(tap.cursor(ID), Some(10));
    let snapshot = tap.read(ID, PcmTapWindow::recent(2)).expect("snapshot");
    assert_eq!(snapshot.first_frame, 8);
    assert_eq!(snapshot.frames, [8.0, -8.0, 9.0, -9.0]);
}

#[test]
fn backward_position_jumps_without_a_loop_are_seeks() {
    let (tap, feed) = registered(32);
    feed.attach_decoded(decoded(200), false);
    feed.set_source_position(100);
    assert_eq!(tap.cursor(ID), Some(100));

    // No loop region: a backward jump is the stream being repositioned, and
    // the cursor follows it instead of counting forward.
    feed.set_source_position(30);
    assert_eq!(tap.cursor(ID), Some(30));
    assert_eq!(
        tap.read(ID, PcmTapWindow::ahead(1))
            .expect("snapshot")
            .frames,
        [30.0, -30.0]
    );
}

#[test]
fn a_contended_push_is_dropped_without_blocking() {
    let (tap, feed) = registered(16);
    let _ = tap.read(ID, PcmTapWindow::ahead(0));
    let instance = instance_of(&tap);

    // A reader holding the buffer lock must never stall the producer.
    let held = instance.lock_buffers();
    feed.push(&stereo(0..8));
    assert_eq!(feed.dropped_pushes(), 1);
    drop(held);

    // The next push resumes at the correct coordinate; the gap is silence.
    feed.push(&stereo(8..16));
    feed.advance_rendered(16);
    let snapshot = tap.read_recent(ID, 16).expect("snapshot");
    assert_eq!(snapshot.first_frame, 0);
    assert_eq!(snapshot.available_frames, 8);
    assert!(snapshot.frames[..8 * 2].iter().all(|sample| *sample == 0.0));
    assert_eq!(snapshot.frames[8 * 2..], stereo(8..16));
}

#[test]
fn register_replaces_the_instance_and_remove_clears_it() {
    let tap = PcmTap::new(8);
    let first = tap.register(ID, spec(2));
    assert!(tap.contains(ID));

    let second = tap.register(
        ID,
        PcmAudioSpec {
            sample_rate: 44_100,
            channels: 1,
        },
    );
    assert_eq!(
        tap.spec(ID),
        Some(PcmAudioSpec {
            sample_rate: 44_100,
            channels: 1
        })
    );
    assert_eq!(second.id(), ID);
    assert_eq!(first.id(), ID);
    // The retired feed no longer reaches the registered instance.
    first.push(&stereo(0..4));
    assert_eq!(
        tap.read(ID, PcmTapWindow::ahead(4))
            .expect("snapshot")
            .available_frames,
        0
    );

    assert!(tap.remove(ID));
    assert!(!tap.contains(ID));
    assert!(tap.read(ID, PcmTapWindow::ahead(4)).is_none());
    assert!(tap.cursor(ID).is_none());
}

#[test]
fn arming_mid_playback_starts_the_clock_at_the_arming_point() {
    let (tap, feed) = registered(16);
    // Playback has been running: positions were reported and decoded frames
    // were pushed before anybody read.
    feed.set_source_position(4_000);
    feed.set_source_position(4_800);
    feed.push(&stereo(0..8));
    assert!(!instance_of(&tap).ring_allocated());

    let snapshot = tap.read(ID, PcmTapWindow::ahead(1)).expect("snapshot");
    assert_eq!(
        snapshot.cursor, 0,
        "the clock starts where the tap was armed"
    );
    assert_eq!(snapshot.available_frames, 0);

    // Frames decoded from here on are readable at the new origin.
    feed.push(&stereo(8..16));
    feed.advance_rendered(8);
    let snapshot = tap.read_recent(ID, 8).expect("snapshot");
    assert_eq!(snapshot.cursor, 8);
    assert_eq!(snapshot.frames, stereo(8..16));

    // The stream position stays absolute even though the clock restarted.
    feed.set_source_position(4_900);
    assert_eq!(
        tap.read(ID, PcmTapWindow::ahead(1))
            .expect("snapshot")
            .source_frame,
        Some(4_900)
    );
}

#[test]
fn window_lengths_are_clamped() {
    let window = PcmTapWindow {
        back_frames: u32::MAX,
        ahead_frames: u32::MAX,
    };
    assert_eq!(window.frames(), MAX_READ_FRAMES);
    let (tap, feed) = registered(8);
    let _ = tap.read(ID, PcmTapWindow::ahead(0));
    feed.advance_rendered(1);
    let snapshot = tap.read(ID, window).expect("snapshot");
    assert_eq!(snapshot.frames.len(), MAX_READ_FRAMES as usize * 2);
}
