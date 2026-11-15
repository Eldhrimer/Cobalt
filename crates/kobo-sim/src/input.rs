//! Bounded synthetic evdev replay through Cobalt's real HAL decoders.
use kobo_abi::input as ev;
use kobo_hal::{gesture::HoldTracker, gpio, InputEvent32, TouchDecoder, TouchEvent};
use kobo_profile::PanelPose;
use std::io;

#[derive(Clone, Debug)]
pub struct Replay {
    decoder: TouchDecoder,
    holds: HoldTracker,
    forward_is_194: bool,
    raw_events: u64,
}
impl Replay {
    pub fn new(pose: &PanelPose<'_>) -> Self {
        Self {
            decoder: TouchDecoder::default(),
            holds: HoldTracker::default(),
            forward_is_194: pose.rotation() % 4 == pose.profile().reference_rotation % 4,
            raw_events: 0,
        }
    }
    pub fn touch(
        &mut self,
        source: &str,
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool)>> {
        let events = parse(source)?;
        let mut next = self.clone();
        let decoded = events
            .iter()
            .filter_map(|event| {
                next.raw_events = next.raw_events.saturating_add(1);
                let event = next.decoder.push(*event, pose)?;
                Some((event, next.holds.observe(event, millis)))
            })
            .collect::<Vec<_>>();
        if decoded
            .iter()
            .filter(|(event, _)| matches!(event, TouchEvent::Up { .. }))
            .count()
            > 1
        {
            return Err(io::Error::other(
                "split separate gestures into separate input steps so app callbacks can settle",
            ));
        }
        *self = next;
        Ok(decoded)
    }
    pub fn gpio(&mut self, source: &str) -> io::Result<Option<bool>> {
        let events = parse(source)?;
        if events.len() != 1 {
            return Err(io::Error::other("one GPIO event per step"));
        }
        self.raw_events = self.raw_events.saturating_add(1);
        Ok(match gpio::decode(events[0]) {
            Some(gpio::GpioEvent::Button {
                button: button @ (gpio::Button::Page193 | gpio::Button::Page194),
                pressed: true,
            }) => Some((button == gpio::Button::Page194) == self.forward_is_194),
            Some(gpio::GpioEvent::Orientation(gpio::Orientation::PortraitUp)) => {
                self.forward_is_194 = true;
                None
            }
            Some(gpio::GpioEvent::Orientation(gpio::Orientation::PortraitDown)) => {
                self.forward_is_194 = false;
                None
            }
            _ => None,
        })
    }
    pub fn resynchronize(&mut self, state: &str) -> io::Result<()> {
        let snapshot = match state {
            "released" => Some(ev::TouchSnapshot {
                slot: Some(0),
                active: false,
            }),
            "active" => Some(ev::TouchSnapshot {
                slot: Some(0),
                active: true,
            }),
            "unknown" => None,
            _ => {
                return Err(io::Error::other(
                    "resync expects released, active or unknown",
                ))
            }
        };
        if !self.decoder.needs_resynchronization() {
            return Err(io::Error::other(
                "resync needs a report boundary after lost input",
            ));
        }
        self.decoder.resynchronize(snapshot);
        Ok(())
    }
    pub fn tap(
        &mut self,
        x: u32,
        y: u32,
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool)>> {
        if !self.decoder.is_quiescent() {
            return Err(io::Error::other(
                "finish or resynchronize the active raw gesture before tapping",
            ));
        }
        let (x, y) = pose
            .display_to_touch(x, y)
            .ok_or_else(|| io::Error::other("touch is outside the selected profile"))?;
        self.touch(
            &format!("3 47 0;3 57 1;3 53 {x};3 54 {y};0 0 0;3 57 -1;0 0 0"),
            pose,
            millis,
        )
    }
    /// A finger that lands, stays still for the runtime's hold time, and lifts.
    ///
    /// The same decoder and the same `HoldTracker` a real finger goes through:
    /// the touch lands at `millis`, and lifts `HOLD_MILLIS` later, so the lift
    /// is reported as a hold exactly as the device reports one.
    pub fn hold(
        &mut self,
        x: u32,
        y: u32,
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool)>> {
        if !self.decoder.is_quiescent() {
            return Err(io::Error::other(
                "finish or resynchronize the active raw gesture before holding",
            ));
        }
        let (x, y) = pose
            .display_to_touch(x, y)
            .ok_or_else(|| io::Error::other("touch is outside the selected profile"))?;
        let mut events = self.touch(
            &format!("3 47 0;3 57 1;3 53 {x};3 54 {y};0 0 0"),
            pose,
            millis,
        )?;
        events.extend(self.touch(
            "3 57 -1;0 0 0",
            pose,
            millis.saturating_add(kobo_hal::gesture::HOLD_MILLIS),
        )?);
        Ok(events)
    }
    /// A finger that lands, stays still for the hold time, slides to a second
    /// point and lifts there: what a reader does to select a run of words.
    ///
    /// Each event comes with the time it happened, from `millis` on. The
    /// slide is reported as a few moves along the way, as a panel reports
    /// one, so a selection grows under it word by word.
    pub fn drag(
        &mut self,
        from: (u32, u32),
        to: (u32, u32),
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool, u64)>> {
        let (x1, y1) = self.touch_point(from, pose, "dragging")?;
        let (x2, y2) = pose
            .display_to_touch(to.0, to.1)
            .ok_or_else(|| io::Error::other("touch is outside the selected profile"))?;
        let mut next = self.clone();
        let mut events = timed(
            next.touch(
                &format!("3 47 0;3 57 1;3 53 {x1};3 54 {y1};0 0 0"),
                pose,
                millis,
            )?,
            millis,
        );
        let held = millis.saturating_add(kobo_hal::gesture::HOLD_MILLIS);
        let mut at = held;
        for step in 1..=DRAG_STEPS {
            let along = |from: i32, to: i32| {
                let from = i64::from(from);
                let to = i64::from(to);
                from + (to - from) * i64::from(step) / i64::from(DRAG_STEPS)
            };
            at = held.saturating_add(u64::from(step) * DRAG_STEP_MILLIS);
            events.extend(timed(
                next.touch(
                    &format!("3 53 {};3 54 {};0 0 0", along(x1, x2), along(y1, y2)),
                    pose,
                    at,
                )?,
                at,
            ));
        }
        events.extend(timed(next.touch("3 57 -1;0 0 0", pose, at)?, at));
        *self = next;
        Ok(events)
    }
    /// Two quick taps on one point, close enough together to be a double tap.
    pub fn double_tap(
        &mut self,
        x: u32,
        y: u32,
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool, u64)>> {
        let (x, y) = self.touch_point((x, y), pose, "tapping")?;
        let mut next = self.clone();
        let tap = format!("3 47 0;3 57 1;3 53 {x};3 54 {y};0 0 0;3 57 -1;0 0 0");
        let second = millis.saturating_add(DOUBLE_TAP_GAP_MILLIS);
        let mut events = timed(next.touch(&tap, pose, millis)?, millis);
        events.extend(timed(next.touch(&tap, pose, second)?, second));
        *self = next;
        Ok(events)
    }
    /// A quick tap, then a second contact on the same point held still for
    /// the hold time.
    pub fn double_hold(
        &mut self,
        x: u32,
        y: u32,
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool, u64)>> {
        let (x, y) = self.touch_point((x, y), pose, "tapping")?;
        let mut next = self.clone();
        let land = format!("3 47 0;3 57 1;3 53 {x};3 54 {y};0 0 0");
        let second = millis.saturating_add(DOUBLE_TAP_GAP_MILLIS);
        let lift = second.saturating_add(kobo_hal::gesture::HOLD_MILLIS);
        let mut events = timed(
            next.touch(&format!("{land};3 57 -1;0 0 0"), pose, millis)?,
            millis,
        );
        events.extend(timed(next.touch(&land, pose, second)?, second));
        events.extend(timed(next.touch("3 57 -1;0 0 0", pose, lift)?, lift));
        *self = next;
        Ok(events)
    }
    fn touch_point(
        &self,
        (x, y): (u32, u32),
        pose: &PanelPose<'_>,
        doing: &str,
    ) -> io::Result<(i32, i32)> {
        if !self.decoder.is_quiescent() {
            return Err(io::Error::other(format!(
                "finish or resynchronize the active raw gesture before {doing}"
            )));
        }
        pose.display_to_touch(x, y)
            .ok_or_else(|| io::Error::other("touch is outside the selected profile"))
    }
    /// Independently authored fixture coordinate, not a device event capture.
    pub fn fixture_tap(
        &mut self,
        pose: &PanelPose<'_>,
        millis: u64,
    ) -> io::Result<Vec<(TouchEvent, bool)>> {
        self.tap(pose.width() / 4, pose.height() / 3, pose, millis)
    }
    pub fn is_quiescent(&self) -> bool {
        self.decoder.is_quiescent()
    }
    pub fn json(&self) -> kobo_json::Value {
        kobo_json::ObjectBuilder::new()
            .set("source", "synthetic evdev replay")
            .set("rawEvents", self.raw_events.to_string())
            .set("quiescent", self.decoder.is_quiescent())
            .set(
                "needsResynchronization",
                self.decoder.needs_resynchronization(),
            )
            .set(
                "forwardKey",
                if self.forward_is_194 {
                    194_u32
                } else {
                    193_u32
                },
            )
            .build()
    }
}
/// How many moves a synthetic drag is reported as, and how far apart.
const DRAG_STEPS: u32 = 8;
const DRAG_STEP_MILLIS: u64 = 20;
/// How long after the first tap of a synthetic double tap the second lands:
/// well inside the double-tap window.
const DOUBLE_TAP_GAP_MILLIS: u64 = 120;

fn timed(events: Vec<(TouchEvent, bool)>, millis: u64) -> Vec<(TouchEvent, bool, u64)> {
    events
        .into_iter()
        .map(|(event, held)| (event, held, millis))
        .collect()
}

fn parse(source: &str) -> io::Result<Vec<InputEvent32>> {
    if source.len() > 4096 {
        return Err(io::Error::other("input step exceeds 4096 bytes"));
    }
    let mut events = Vec::new();
    for record in source.split(';') {
        if events.len() == 64 {
            return Err(io::Error::other("input step exceeds 64 events"));
        }
        let values = record.split_whitespace().collect::<Vec<_>>();
        let event = match values.as_slice() {
            [kind, code, value] => kind
                .parse()
                .ok()
                .zip(code.parse().ok())
                .zip(value.parse().ok())
                .map(|((kind, code), value)| InputEvent32 { kind, code, value }),
            _ => None,
        }
        .ok_or_else(|| {
            io::Error::other("input events use TYPE CODE VALUE, separated by semicolons")
        })?;
        events.push(event);
    }
    Ok(events)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_taps_reach_same_display_position_on_three_profile_poses() {
        for id in ["clara-hd-376", "libra-h2o-384", "libra-2-388"] {
            let profile = kobo_profile::SUPPORTED_PROFILES
                .iter()
                .copied()
                .find(|p| p.id == id)
                .unwrap();
            let pose = PanelPose::reference(profile);
            let mut replay = Replay::new(&pose);
            let events = replay.fixture_tap(&pose, 0).unwrap();
            assert_eq!(
                events.last(),
                Some(&(
                    TouchEvent::Up {
                        x: pose.width() / 4,
                        y: pose.height() / 3
                    },
                    false
                ))
            );
            assert!(replay.is_quiescent());
        }
    }
    #[test]
    fn real_decoder_handles_taps_holds_lost_input_and_explicit_recovery() {
        let pose = &super::super::POSE;
        let mut replay = Replay::new(pose);
        let tap = replay.tap(100, 200, pose, 0).unwrap();
        assert!(matches!(
            tap.as_slice(),
            [
                (TouchEvent::Down { .. }, false),
                (TouchEvent::Up { .. }, false)
            ]
        ));
        let (x, y) = pose.display_to_touch(100, 200).unwrap();
        replay
            .touch(&format!("3 47 0;3 57 2;3 53 {x};3 54 {y};0 0 0"), pose, 0)
            .unwrap();
        assert!(matches!(
            replay.touch("3 57 -1;0 0 0", pose, 500).unwrap().as_slice(),
            [(TouchEvent::Up { .. }, true)]
        ));
        let dropped = replay.touch("0 3 0;0 0 0", pose, 1000).unwrap();
        assert_eq!(dropped, vec![(TouchEvent::Cancel, false)]);
        assert!(replay.tap(100, 200, pose, 1000).is_err());
        replay.resynchronize("active").unwrap();
        assert!(replay.tap(100, 200, pose, 1000).is_err());
        replay.touch("0 0 0", pose, 1000).unwrap();
        replay.resynchronize("released").unwrap();
        assert_eq!(replay.tap(100, 200, pose, 1000).unwrap().len(), 2);
    }
    #[test]
    fn a_synthetic_hold_lands_then_lifts_after_the_hold_time_as_a_hold() {
        let pose = &super::super::POSE;
        let mut replay = Replay::new(pose);
        let events = replay.hold(100, 200, pose, 1000).unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                (TouchEvent::Down { .. }, false),
                (TouchEvent::Up { .. }, true)
            ]
        ));
        assert!(
            replay.is_quiescent(),
            "the finger is lifted, ready for the next gesture"
        );
        // A tap straight after is an ordinary tap, not a second hold.
        let tap = replay.tap(100, 200, pose, 1000).unwrap();
        assert!(matches!(
            tap.as_slice(),
            [_, (TouchEvent::Up { .. }, false)]
        ));
    }
    #[test]
    fn synthetic_selection_gestures_land_move_and_lift_on_time() {
        let pose = &super::super::POSE;
        let mut replay = Replay::new(pose);
        let drag = replay.drag((100, 200), (300, 260), pose, 1000).unwrap();
        assert!(matches!(
            drag.first(),
            Some((TouchEvent::Down { x: 100, y: 200 }, false, 1000))
        ));
        let moves = drag
            .iter()
            .filter(|(event, _, _)| matches!(event, TouchEvent::Move { .. }))
            .collect::<Vec<_>>();
        assert!(!moves.is_empty());
        assert!(moves
            .iter()
            .all(|(_, _, at)| *at > 1000 + kobo_hal::gesture::HOLD_MILLIS));
        assert!(matches!(
            drag.last(),
            Some((TouchEvent::Up { x: 300, y: 260 }, _, _))
        ));
        assert!(replay.is_quiescent());

        let double = replay.double_tap(100, 200, pose, 5000).unwrap();
        let ups = double
            .iter()
            .filter(|(event, _, _)| matches!(event, TouchEvent::Up { .. }))
            .map(|(_, held, at)| (*held, *at))
            .collect::<Vec<_>>();
        assert_eq!(
            ups,
            vec![(false, 5000), (false, 5000 + DOUBLE_TAP_GAP_MILLIS)]
        );

        let held = replay.double_hold(100, 200, pose, 9000).unwrap();
        assert!(matches!(
            held.last(),
            Some((TouchEvent::Up { .. }, true, at))
                if *at == 9000 + DOUBLE_TAP_GAP_MILLIS + kobo_hal::gesture::HOLD_MILLIS
        ));
        assert!(replay.is_quiescent());
    }
    #[test]
    fn page_keys_use_hal_edges_and_portrait_mapping_and_bad_records_do_not_mutate() {
        let mut replay = Replay::new(&super::super::POSE);
        replay.gpio("4 3 24").unwrap(); // HAL PortraitUp.
        assert_eq!(replay.gpio("1 194 1").unwrap(), Some(true));
        assert_eq!(replay.gpio("1 194 0").unwrap(), None);
        assert_eq!(replay.gpio("1 194 2").unwrap(), None);
        replay.gpio("4 3 23").unwrap();
        assert_eq!(replay.gpio("1 194 1").unwrap(), Some(false));
        let before = replay.json();
        assert!(replay
            .touch("3 47 0;invalid", &super::super::POSE, 0)
            .is_err());
        assert_eq!(replay.json(), before);
    }
}
