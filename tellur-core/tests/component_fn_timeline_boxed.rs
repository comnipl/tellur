use tellur_core::timeline_component::{
    Arrangement, AudioBlockMut, AudioEffects, AudioRenderContext, AudioRenderRequest, NodeKind,
    TimelineComponent,
};
use tellur_core::timeline_container::Timeline;

#[derive(Clone, PartialEq, Eq, Hash)]
struct ProbeAudio;

impl TimelineComponent for ProbeAudio {
    fn duration(&self) -> Option<f64> {
        Some(2.0)
    }

    fn render_audio_block(&self, mut block: AudioBlockMut<'_>, _ctx: &mut AudioRenderContext) {
        block.samples_mut().fill(1.0);
    }

    fn arrangement(&self, offset: f64) -> Arrangement {
        Arrangement {
            kind: NodeKind::Audio,
            label: "probe".into(),
            name: None,
            source: None,
            start: offset,
            end: offset + 2.0,
            trim: None,
            triggers: Vec::new(),
            children: Vec::new(),
        }
    }
}

#[tellur_core::component(timeline)]
fn BoxedAudio(child: Box<dyn TimelineComponent + Send>) -> impl TimelineComponent {
    Timeline::builder().child(child).build()
}

#[test]
fn function_timeline_component_accepts_and_forwards_a_boxed_child() {
    let child: Box<dyn TimelineComponent + Send> = ProbeAudio
        .gain_envelope((0.0, 0.0), (1.0, 1.0))
        .gain_envelope((0.0, 0.5), (1.0, 0.5))
        .into();
    let component = BoxedAudio::builder().child(child).build();

    assert_eq!(component.measure(), Some(2.0));

    let arrangement = component.arrangement(3.0);
    assert_eq!(arrangement.kind, NodeKind::Timeline);
    assert_eq!(arrangement.name.as_deref(), Some("BoxedAudio"));
    assert_eq!(arrangement.start, 3.0);
    assert_eq!(arrangement.end, 5.0);
    assert_eq!(arrangement.children.len(), 1);
    assert_eq!(arrangement.children[0].kind, NodeKind::Audio);

    let request = AudioRenderRequest::new(0, 4, 2, 1);
    let mut samples = vec![0.0; request.sample_len()];
    component.render_audio_block(
        AudioBlockMut::new(request, &mut samples),
        &mut AudioRenderContext::new(),
    );
    assert_eq!(samples, vec![0.0, 0.25, 0.5, 0.5]);
}
