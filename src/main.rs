use glib::translate::ToGlibPtr;
use gst::prelude::*;
use log::{debug, info};
use std::fs::File;
use std::io::Write;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn send_splice<C>(element: &gst::Element, gst_sit: C)
where
    C: FnOnce() -> *mut gst_mpegts::GstMpegtsSCTESIT,
{
    let sit = gst_sit();
    assert!(!sit.is_null());
    unsafe {
        let section = gst_mpegts::gst_mpegts_section_from_scte_sit(sit, 500);
        gst_mpegts::gst_mpegts_section_send_event(section, element.to_glib_none().0);
        gst::ffi::gst_mini_object_unref(section as _);
    };
}

fn send_splice_in(element: &gst::Element, event_id: u32, time: gst::ClockTime) {
    info!("Sending Splice In event: {} @ {}", event_id, time.display());
    send_splice(element, || unsafe {
        gst_mpegts::gst_mpegts_scte_splice_in_new(event_id, time.nseconds())
    })
}

fn send_splice_out(element: &gst::Element, event_id: u32, time: gst::ClockTime) {
    info!(
        "Sending Splice Out event: {} @ {}",
        event_id,
        time.display()
    );
    send_splice(element, || unsafe {
        gst_mpegts::gst_mpegts_scte_splice_out_new(event_id, time.nseconds(), 0)
    })
}

#[derive(Clone)]
pub struct EventId(Arc<Mutex<u32>>);

impl EventId {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(0)))
    }

    pub fn next(&self) -> u32 {
        let mut counter = self.0.lock().unwrap();
        *counter += 1;
        *counter
    }
}


fn main() -> eyre::Result<()> {
    pretty_env_logger::init_timed();
    gst::init()?;
    unsafe {
        gst_mpegts::gst_mpegts_initialize();
    }

    // ! rtpmp2tpay ! udpsink sync=true host=184.73.103.62 port=5000
    let pipeline = gst::parse_launch(
        r#"

        audiotestsrc is-live=true ! audioconvert ! avenc_aac bitrate=128000 ! queue ! mux.

        videotestsrc is-live=true ! video/x-raw,framerate=30/1,width=1280,height=720 ! timeoverlay ! x264enc tune=zerolatency name=encoder

        encoder. ! video/x-h264,profile=main ! queue ! mpegtsmux name=mux scte-35-pid=500 scte-35-null-interval=450000

        mux. ! filesink sync=true location=out.ts

    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();

    info!("Starting pipeline...");

    let event_counter = EventId::new();

    // Every 60 seconds we will loop on an ad scheduling process..
    glib::timeout_add(Duration::from_secs(60), {
        let pipeline_weak = pipeline.downgrade();
        let event_counter = event_counter.clone();
        move || {
            if let Some(pipeline) = pipeline_weak.upgrade() {
                let muxer = pipeline.by_name("mux").unwrap();

                // We need to notify a specific time in the stream where the SCTE-35 marker
                // is, so we use the pipeline running time to base our timing calculations
                let now = pipeline.current_running_time().unwrap();

                // How much ahead should the ad be inserted, we say 0 seconds in the future (immediate)
                let ahead = gst::ClockTime::from_seconds(0);

                // Trigger the Splice Out event in the SCTE-35 stream
                send_splice_out(&muxer, event_counter.next(), now + ahead);

                // Now we add a timed call for the duration of the ad from now to indicate via
                // splice in that the stream can go back to normal programming.
                glib::timeout_add(Duration::from_secs(30), {
                    let muxer_weak = muxer.downgrade();
                    let event_counter = event_counter.clone();
                    move || {
                        if let Some(muxer) = muxer_weak.upgrade() {
                            let now = muxer.current_running_time().unwrap();
                            send_splice_in(&muxer, event_counter.next(), now + ahead);
                        }
                        // This shall not run again
                        glib::Continue(false)
                    }
                });
            }
            // Run this again after the timeout...
            glib::Continue(true)
        }
    });

    let context = glib::MainContext::default();
    let main_loop = glib::MainLoop::new(Some(&context), false);

    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();
    bus.add_watch({
        let main_loop = main_loop.clone();
        let pipeline_weak = pipeline.downgrade();
        move |_, msg| {
            use gst::MessageView;

            let main_loop = &main_loop;
            match msg.view() {
                MessageView::Eos(..) => main_loop.quit(),
                MessageView::Error(err) => {
                    println!(
                        "Error from {:?}: {} ({:?})",
                        err.src().map(|s| s.path_string()),
                        err.error(),
                        err.debug()
                    );
                    main_loop.quit();
                }
                MessageView::StateChanged(s) => {
                    if let Some(pipeline) = pipeline_weak.upgrade() {
                        if s.src().map(|e| e == pipeline).unwrap_or(false) {
                            debug!("Writing dot file for status: {:?}", s.current());

                            let mut file = File::create(format!("Pipeline-{:?}.dot", s.current())).unwrap();
                            let dot_data = pipeline.debug_to_dot_data(
                                gst::DebugGraphDetails::all(),
                            );
                            file.write_all(dot_data.as_bytes()).unwrap();
                        }
                    }
                }
                _ => (),
            };

            glib::Continue(true)
        }
    })
    .expect("Failed to add bus watch");

    ctrlc::set_handler({
        let main_loop = main_loop.clone();
        move || {
            main_loop.quit();
        }
    })?;

    main_loop.run();
    bus.remove_watch().unwrap();

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}
