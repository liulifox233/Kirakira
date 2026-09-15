//! The live player session: a real runtime, not a re-run of the static scene
//! constructor.
//!
//! The fixture ([`support::player_fixture::blinking_motion`]) is a motion whose
//! face layer is parameterised by a variable and whose authored `EPEyeControl`
//! writes that variable every tick. The static sampler cannot see any of that;
//! [`krkr_emote::MotionPlayer`] can, and these tests pin both halves.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use krkr_emote::{Canvas, Motion, MotionPlayer, SpriteBlend, TextureCache, Tint, render_draw_list};
use support::player_fixture::{BODY_STEP_TICK, blinking_motion, blinking_motion_with_body};

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn rendered(motion: &Arc<Motion>, items: &[krkr_emote::MotionDrawItem]) -> (usize, u64) {
    let mut canvas = Canvas::new(64, 64);
    let mut textures = TextureCache::new(Arc::clone(motion));
    let mut items = items.to_vec();
    for item in items.iter_mut() {
        item.world_transform[4] += 32.0;
        item.world_transform[5] += 32.0;
    }
    let report = render_draw_list(&mut canvas, &items, &mut textures, Tint::IDENTITY);
    let non_zero = canvas
        .pixels()
        .chunks_exact(4)
        .filter(|pixel| pixel[3] != 0)
        .count();
    assert_eq!(report.drawn, 1, "the fixture's single layer draws");
    (non_zero, fnv1a(canvas.pixels()))
}

/// The static constructor samples the parameterised layer at local time 0 for
/// every tick — the variable the eye control writes is never supplied, so the
/// open eye stands for the whole animation.
#[test]
fn the_static_sampler_never_sees_the_eye_control() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let at_zero = motion.draw_list("idle", 0.0).expect("tick 0");
    let at_thirty = motion.draw_list("idle", 30.0).expect("tick 30");
    assert_eq!(at_zero.len(), 1);
    assert_eq!(at_zero[0].icon, "eye_open");
    assert_eq!(
        at_zero[0].icon, at_thirty[0].icon,
        "without a player the sampled face never changes"
    );
    assert_eq!(
        rendered(&motion, &at_zero),
        rendered(&motion, &at_thirty),
        "…and neither do the pixels"
    );
}

/// The live player runs the control pass, so the same motion's face changes as
/// the clock advances: the blink timer drives the variable, the variable
/// selects the layer's sample time, and the sample time picks the icon.
#[test]
fn a_live_player_blinks_on_its_own_clock() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let mut player = MotionPlayer::with_motion(&motion, "idle").expect("player");

    let first = player.draw_list();
    assert_eq!(first[0].icon, "eye_open", "the authored default is open");

    let mut icons = Vec::new();
    let mut values = Vec::new();
    for _ in 0..40 {
        player.advance_ticks(1.0).expect("tick");
        icons.push(player.draw_list()[0].icon.clone());
        values.push(player.variables()["face"]);
    }
    let closed_at = icons
        .iter()
        .position(|icon| icon == "eye_closed")
        .unwrap_or_else(|| panic!("the blink timer never closed the eye; values={values:?}"));
    assert!(
        icons.iter().any(|icon| icon == "eye_open"),
        "the eye opens again after the blink"
    );

    // The frame at the closed tick really is a different image.
    let mut closed = MotionPlayer::with_motion(&motion, "idle").expect("player");
    for _ in 0..=closed_at {
        closed.advance_ticks(1.0).expect("tick");
    }
    let closed_items = closed.draw_list();
    assert_eq!(closed_items[0].icon, "eye_closed");
    assert_ne!(
        rendered(&motion, &first),
        rendered(&motion, &closed_items),
        "the frame itself changed"
    );

    // An immediate variable write reaches the next sample without waiting for
    // the control pass.
    player.set_variable("face", 0.0).expect("reset");
    assert_eq!(player.draw_list()[0].icon, "eye_open");
    player.set_variable("face", 1.0).expect("close");
    assert_eq!(player.draw_list()[0].icon, "eye_closed");
    assert_eq!(player.variables()["face"], 1.0);
}

/// The session's animation clock: `set_motion` puts the new animation back to
/// its own time 0 (the reference switches motions with `play`, not with a
/// property write — see the method's docs), while the player's own state, which
/// the controls and timelines run on, is untouched by the switch.
#[test]
fn set_motion_restarts_the_animations_clock() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let mut player = MotionPlayer::with_motion(&motion, "idle").expect("player");

    for _ in 0..40 {
        player.advance_ticks(1.0).expect("tick");
    }
    assert_eq!(
        player.elapsed_ticks(),
        40.0,
        "the animation has run 40 ticks"
    );
    // The eye control runs on the player's clock, not the animation's.
    let variables_before = player.variables();

    player.set_motion("lit").expect("switch");
    assert_eq!(player.motion(), "lit");
    assert_eq!(
        player.elapsed_ticks(),
        0.0,
        "the new animation starts at its own time 0"
    );
    assert_eq!(
        player.variables(),
        variables_before,
        "the session's variables survive the switch"
    );

    // …and it advances from there, not from the session's accumulated 40.
    player.advance_ticks(3.0).expect("tick");
    assert_eq!(player.elapsed_ticks(), 3.0);

    // The sample time really is that clock: the `lit` frame carries a mask,
    // and the idling `idle` animation's parameterised layer is back at local
    // time 0 rather than at 43 ticks of the old animation.
    player.set_motion("idle").expect("switch back");
    assert_eq!(player.elapsed_ticks(), 0.0);
    assert_eq!(
        player.draw_list()[0].icon,
        "eye_open",
        "the restarted animation samples from its first frame"
    );
}

/// The draw list a player produces carries the per-sprite state the reference
/// keeps (`bm`, `bp`, the four corner colours) — the boundary the old adapter
/// dropped on the floor.
///
/// The fixture's `lit` animation *authors* all three (mask-gated the way a
/// game's frames are), so this fails if `from_sprite` stops reading them: the
/// item would carry the defaults (`0x10`, `0.0`, the neutral gray) instead of
/// the authored values, on both the static sampler's and the session's path.
#[test]
fn the_draw_list_carries_the_authored_blend_and_corner_state() {
    use support::player_fixture::{LIT_BLEND_MODE, LIT_BLEND_PARAMETER, LIT_CORNER_COLORS};

    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let authored = |item: &krkr_emote::MotionDrawItem| {
        assert_eq!(
            item.blend_mode, LIT_BLEND_MODE as u32,
            "the frame's `bm` must survive the draw-list boundary"
        );
        assert_eq!(
            item.blend_parameter, LIT_BLEND_PARAMETER as f32,
            "the frame's `bp` must survive the draw-list boundary"
        );
        assert_eq!(
            item.corner_colors,
            LIT_CORNER_COLORS.map(|value| value as u32),
            "the frame's four corner colours must survive, corner by corner"
        );
        assert_eq!(
            SpriteBlend::of(item.blend_mode),
            SpriteBlend::Mul,
            "…and the renderer's table must read them"
        );
    };

    // The static sampler crosses the same boundary.
    let static_items = motion.draw_list("lit", 0.0).expect("lit samples");
    assert_eq!(static_items.len(), 1);
    authored(&static_items[0]);

    // …and so does the live session's frame.
    let player = MotionPlayer::with_motion(&motion, "lit").expect("player");
    let items = player.draw_list();
    assert_eq!(items.len(), 1);
    authored(&items[0]);
}

/// One frame's icons, in draw order.
fn icons(player: &MotionPlayer) -> Vec<String> {
    player
        .draw_list()
        .into_iter()
        .map(|item| item.icon)
        .collect()
}

/// A host that keeps its own position model drives the session with
/// `advance_and_sample`: the session's own clock takes the delta — the control
/// pass and every timed write move with it — while the frame comes from the
/// sample position the host chose. That split is what the plugin's loop wrap
/// needs: a motion re-entering at its `loopTime` restarts the frame without
/// rewinding the controls, which run on the player's clock.
#[test]
fn a_host_position_and_the_sessions_clock_are_separate() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion_with_body()).expect("fixture loads"));
    let mut player = MotionPlayer::with_motion(&motion, "idle").expect("player");

    // A timed write gives the session's own clock something to move: 40 ticks
    // of a 20-tick write lands on the target.
    assert!(
        player.write_variable_timed("face", 1.0, 20.0, 0.0),
        "`face` is an authored variable"
    );
    player
        .advance_and_sample(40.0, 0.0, &BTreeMap::new())
        .expect("step");

    assert_eq!(
        player.elapsed_ticks(),
        0.0,
        "the animation's sample time is the host's, not the clock's"
    );
    assert_eq!(
        player.variable("face"),
        Some(1.0),
        "the session's clock ran the write out"
    );
    let sampled = icons(&player);
    assert!(
        sampled.iter().any(|icon| icon == "body_early"),
        "the frame is the host's sample (tick 0), not the clock's: {sampled:?}"
    );
    assert!(
        sampled.iter().any(|icon| icon == "eye_closed"),
        "…while the control's output is where the clock put it: {sampled:?}"
    );

    // Re-sampling re-keys the time-driven layer without touching the clock.
    player.sample_at(35.0, &BTreeMap::new()).expect("sample");
    let sampled = icons(&player);
    assert!(
        sampled.iter().any(|icon| icon == "body_late"),
        "the position write past tick {BODY_STEP_TICK} re-samples the body: {sampled:?}"
    );
    assert_eq!(
        player.variable("face"),
        Some(1.0),
        "a sample is not an advance"
    );
}

/// A host record for a name the session knows does not shadow the player's own
/// table: `face` belongs to the authored eye control, so a host map entry for it
/// cannot pin the eye open or shut behind the control's back. The host's records
/// exist for the names the *file* never mentions.
#[test]
fn host_records_do_not_shadow_the_sessions_variables() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let mut player = MotionPlayer::with_motion(&motion, "idle").expect("player");
    let overrides = BTreeMap::from([("face".to_owned(), 1.0)]);
    let items = player.sample_at(0.0, &overrides).expect("sample");
    assert_eq!(
        items[0].icon, "eye_open",
        "the evaluated table wins for a name the file authors"
    );
}

/// `write_variable`/`write_variable_timed` answer whether the session knows the
/// name. A `false` is the host's own record — eluna's table is built from the
/// parse and cannot grow — which the host layers into a sample
/// (`MotionPlayer::sample_at`); the plugin's own
/// `variable_setters_change_the_drawn_frame` is the end-to-end half of that.
#[test]
fn a_write_for_a_name_the_file_never_authors_is_the_hosts() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let mut player = MotionPlayer::with_motion(&motion, "idle").expect("player");
    assert!(player.write_variable("face", 0.5));
    assert!(!player.write_variable("unheard_of", 1.0));
    assert!(!player.write_variable_timed("unheard_of", 1.0, 10.0, 0.0));
    assert_eq!(
        player.variable("unheard_of"),
        None,
        "the session has no record for it"
    );
    assert_eq!(player.variable("face"), Some(0.5));
}
