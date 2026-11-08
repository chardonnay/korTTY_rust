use super::*;

fn sample(at: Instant) -> OutputSample {
    OutputSample {
        first: Some(at),
        last: Some(at),
        mirrored: None,
    }
}

#[test]
fn tab_watch_activity_requires_quiet_and_rearms_after_being_seen() {
    let start = Instant::now();
    let watch = Watch {
        activity: true,
        silence: false,
    };
    let silence = Duration::from_secs(30);
    let time = |seconds| start + Duration::from_secs(seconds);
    let mut state = PaneWatch::new(watch, Some(start), start, silence);
    for second in (1..10).step_by(2) {
        assert!(
            state
                .poll(watch, OutputSample::default(), time(second), silence, false)
                .is_none()
        );
    }
    assert!(matches!(
        state.poll(watch, sample(time(10)), time(10), silence, false),
        Some(Attention::Activity)
    ));

    for second in [12, 15, 18] {
        assert!(
            state
                .poll(watch, OutputSample::default(), time(second), silence, false)
                .is_none()
        );
    }
    assert!(
        state
            .poll(watch, sample(time(20)), time(20), silence, false)
            .is_none()
    );
    assert!(
        state
            .poll(watch, OutputSample::default(), time(21), silence, true)
            .is_none()
    );
    for second in [24, 27] {
        assert!(
            state
                .poll(watch, OutputSample::default(), time(second), silence, false)
                .is_none()
        );
    }
    assert!(matches!(
        state.poll(watch, sample(time(30)), time(30), silence, false),
        Some(Attention::Activity)
    ));
}

#[test]
fn tab_watch_silence_counts_recent_output_once_and_mirrored_echo_does_not_arm() {
    let start = Instant::now();
    let watch = Watch {
        activity: true,
        silence: true,
    };
    let silence = Duration::from_secs(5);
    let time = |seconds| start + Duration::from_secs(seconds);
    let mut state = PaneWatch::new(watch, None, start, silence);
    let echo = OutputSample {
        mirrored: Some(time(1)),
        ..sample(time(1))
    };
    assert!(state.poll(watch, echo, time(2), silence, false).is_none());
    assert!(
        state
            .poll(watch, OutputSample::default(), time(6), silence, false)
            .is_none()
    );
    assert!(
        state
            .poll(watch, sample(time(7)), time(7), silence, false)
            .is_none()
    );
    assert!(matches!(
        state.poll(watch, OutputSample::default(), time(12), silence, false),
        Some(Attention::Silence(_))
    ));
    assert!(
        state
            .poll(watch, OutputSample::default(), time(14), silence, false)
            .is_none()
    );

    let mut state = PaneWatch::new(watch, Some(time(15)), time(17), silence);
    assert!(matches!(
        state.poll(watch, OutputSample::default(), time(20), silence, false),
        Some(Attention::Silence(_))
    ));
    assert!(
        state
            .poll(watch, sample(time(40)), time(40), silence, false)
            .is_none()
    );
}
