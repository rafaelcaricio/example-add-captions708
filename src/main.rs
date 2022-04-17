use glib::translate::ToGlibPtr;
use gst::prelude::*;
use log::{debug, info};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
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

#[derive(Clone, Default)]
pub struct EventId(Arc<Mutex<u32>>);

impl EventId {
    pub fn next(&self) -> u32 {
        let mut counter = self.0.lock().unwrap();
        *counter += 1;
        *counter
    }
}

fn main() -> eyre::Result<()> {
    pretty_env_logger::init_timed();
    gst::init()?;
    gstvideofx::plugin_register_static()?;
    unsafe {
        gst_mpegts::gst_mpegts_initialize();
    }

    //  ! filesink sync=true location=video.ts
    let pipeline = gst::parse_launch(
        r#"

        urisourcebin uri=https://plutolive-msl.akamaized.net/hls/live/2008623/defy/master.m3u8 ! tsdemux name=demux ! queue ! h264parse ! tee name=v

        v. ! queue ! mpegtsmux name=mux scte-35-pid=500 scte-35-null-interval=450000 ! rtpmp2tpay ! udpsink sync=true host=54.225.215.79 port=5000
        v. ! queue ! decodebin ! videoconvert ! imgcmp name=imgcmp location=/Users/rafaelcaricio/Downloads/defy-AD-SLATE-APRIL3022.jpeg ! autovideosink

        demux. ! queue ! aacparse ! mux.
    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();

    info!("Starting pipeline...");

    let context = glib::MainContext::default();
    let main_loop = glib::MainLoop::new(Some(&context), false);

    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();
    bus.add_watch({
        let event_counter = EventId::default();
        let ad_running = Arc::new(AtomicBool::new(false));
        let main_loop = main_loop.clone();
        let pipeline_weak = pipeline.downgrade();
        let imgcmp_weak = pipeline.by_name("imgcmp").unwrap().downgrade();
        let muxer_weak = pipeline.by_name("mux").unwrap().downgrade();
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

                            let mut file =
                                File::create(format!("Pipeline-{:?}.dot", s.current())).unwrap();
                            let dot_data =
                                pipeline.debug_to_dot_data(gst::DebugGraphDetails::all());
                            file.write_all(dot_data.as_bytes()).unwrap();
                        }
                    }
                }
                MessageView::Element(elem_msg) => {
                    if let (Some(pipeline), Some(imgcmp), Some(muxer)) = (
                        pipeline_weak.upgrade(),
                        imgcmp_weak.upgrade(),
                        muxer_weak.upgrade(),
                    ) {
                        info!("Element Message: {:?}", elem_msg);
                        if elem_msg.src().map(|e| e == imgcmp).unwrap_or(false)
                            && elem_msg.message().has_name("image-detected")
                            && !ad_running.load(Ordering::Relaxed)
                        {
                            // We need to notify a specific time in the stream where the SCTE-35 marker
                            // is, so we use the pipeline running time to base our timing calculations
                            let now = pipeline.current_running_time().unwrap();

                            // How much ahead should the ad be inserted, we say 0 seconds in the future (immediate)
                            let ahead = gst::ClockTime::from_seconds(0);

                            // Trigger the Splice Out event in the SCTE-35 stream
                            send_splice_out(&muxer, event_counter.next(), now + ahead);
                            ad_running.store(true, Ordering::Relaxed);
                            info!("Ad started..");

                            // Now we add a timed call for the duration of the ad from now to indicate via
                            // splice in that the stream can go back to normal programming.
                            glib::timeout_add(Duration::from_secs(30), {
                                let muxer_weak = muxer.downgrade();
                                let event_counter = event_counter.clone();
                                let ad_running = Arc::clone(&ad_running);
                                move || {
                                    if let Some(muxer) = muxer_weak.upgrade() {
                                        let now = muxer.current_running_time().unwrap();
                                        send_splice_in(&muxer, event_counter.next(), now + ahead);
                                        ad_running.store(false, Ordering::Relaxed);
                                        info!("Ad ended!")
                                    }
                                    // This shall not run again
                                    glib::Continue(false)
                                }
                            });
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

    debug!("Writing Final dot file");
    let mut file = File::create("Pipeline-Final.dot").unwrap();
    let dot_data = pipeline.debug_to_dot_data(gst::DebugGraphDetails::all());
    file.write_all(dot_data.as_bytes()).unwrap();

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}
