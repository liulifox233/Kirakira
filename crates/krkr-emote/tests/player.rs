//! The live player session: a real runtime, not a re-run of the static scene
//! constructor.
//!
//! The fixture ([`support::player_fixture::blinking_motion`]) is a motion whose
//! face layer is parameterised by a variable and whose authored `EPEyeControl`
//! writes that variable every tick. The static sampler cannot see any of that;
//! [`krkr_emote::MotionPlayer`] can, and these tests pin both halves.

mod support;

use std::sync::Arc;

use krkr_emote::{Canvas, Motion, MotionPlayer, SpriteBlend, TextureCache, Tint, render_draw_list};
use support::player_fixture::blinking_motion;

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

/// The draw list a player produces carries the per-sprite state the reference
/// keeps (`bm`, `bp`, the four corner colours) — the boundary the old adapter
/// dropped on the floor.
#[test]
fn the_draw_list_carries_blend_and_corner_state() {
    let motion = Arc::new(Motion::from_bytes(&blinking_motion()).expect("fixture loads"));
    let player = MotionPlayer::new(&motion).expect("the default animation opens");
    assert_eq!(player.motion(), "idle");
    let item = &player.draw_list()[0];
    assert_eq!(item.blend_mode, 0x10, "an unauthored `bm` is the default");
    assert_eq!(
        item.corner_colors, [0x8080_80FF; 4],
        "an unauthored colour is the neutral MODULATE2X gray"
    );
    assert_eq!(item.blend_parameter, 0.0);
    assert_eq!(SpriteBlend::of(item.blend_mode), SpriteBlend::Over);
}
