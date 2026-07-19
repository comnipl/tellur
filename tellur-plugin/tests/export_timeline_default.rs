use std::sync::atomic::{AtomicUsize, Ordering};

use tellur_core::timeline_container::TimeBox;

static ROOT_BUILDS: AtomicUsize = AtomicUsize::new(0);

tellur_plugin::export_timeline!(
    root = {
        ROOT_BUILDS.fetch_add(1, Ordering::Relaxed);
        TimeBox::builder().duration(2.0).build()
    },
    title = "Default Timeline",
);

#[test]
fn export_timeline_defaults_to_main_id() {
    assert_eq!(ROOT_BUILDS.load(Ordering::Relaxed), 0);
    let collection = tellur_timeline_collection_v9();
    assert_eq!(ROOT_BUILDS.load(Ordering::Relaxed), 1);
    let timelines = collection.timelines();

    assert_eq!(timelines.len(), 1);
    assert_eq!(timelines[0].id, "main");
    assert_eq!(timelines[0].title, "Default Timeline");
    assert_eq!(timelines[0].duration, 2.0);
}
