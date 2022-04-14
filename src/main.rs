use std::fs::File;
use std::io::Write;
use std::process;
use std::sync::{Arc, Mutex};
use gst::prelude::*;
use log::info;
use std::time::Duration;

fn send_splice<C>(element: &gst::Element, gst_sit: C)
where
    C: FnOnce() -> *mut gst_mpegts::GstMpegtsSCTESIT,
{
    let sit = gst_sit();
    unsafe {
        let section = gst_mpegts::gst_mpegts_section_from_scte_sit(sit, 500);
        gst_mpegts::gst_mpegts_section_send_event(section, element.as_ptr());
        gst::ffi::gst_mini_object_unref(section as _);
    };
}

fn send_splice_in(element: &gst::Element, event_id: u32, time: gst::ClockTime) {
    info!("Sending Splice In event: {} @ {}", event_id, time.display());
    send_splice(element, || unsafe {
        gst_mpegts::gst_mpegts_scte_splice_in_new(event_id, time.nseconds())
    })
}

fn send_splice_out(
    element: &gst::Element,
    event_id: u32,
    time: gst::ClockTime,
    duration: gst::ClockTime,
) {
    info!("Sending Splice Out event: {} @ {} for {}", event_id, time.display(), duration.display());
    send_splice(element, || unsafe {
        gst_mpegts::gst_mpegts_scte_splice_out_new(event_id, time.nseconds(), duration.nseconds())
    })
}

fn main() -> eyre::Result<()> {
    pretty_env_logger::init();
    gst::init()?;
    unsafe {
        gst_mpegts::gst_mpegts_initialize();
    }

    let pipeline = gst::parse_launch(
        r#"

        videotestsrc is-live=true ! video/x-raw,framerate=30/1,width=1280,height=720 ! timeoverlay ! x264enc tune=zerolatency name=encoder

        encoder. ! video/x-h264,profile=main ! queue ! mpegtsmux name=mux scte-35-pid=500 scte-35-null-interval=450000 alignment=7 ! rtpmp2tpay ! udpsink sync=true host=184.73.103.62 port=5000

        audiotestsrc is-live=true ! audioconvert ! avenc_aac bitrate=128000 ! queue ! mux.

    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();

    info!("Starting pipeline...");

    let ad_event_counter = Arc::new(Mutex::new(1u32));

    // Every 90 seconds we will loop on an ad scheduling process..
    glib::timeout_add(Duration::from_secs(60), {
        let pipeline_weak = pipeline.downgrade();
        let ad_event_counter = ad_event_counter.clone();
        move || {
            if let Some(pipeline) = pipeline_weak.upgrade() {
                let muxer = pipeline.by_name("mux").unwrap();

                // We need to notify a specific time in the stream where the SCTE-35 marker
                // is, so we use the pipeline running time to base our timing calculations
                let now = pipeline.current_running_time().unwrap();

                // How much ahead should the ad be inserted, we say 5 seconds in the future
                let ahead = gst::ClockTime::from_seconds(5);

                // Schedule an advertisement in 5 seconds from now with a 10s duration
                let ad_duration = gst::ClockTime::from_seconds(10);
                // next event id
                let event_id =  {
                    let mut ad_event_counter = ad_event_counter.lock().unwrap();
                    *ad_event_counter += 1;
                    *ad_event_counter
                };
                send_splice_out(
                    &muxer,
                    event_id,
                    now + ahead,
                    ad_duration.clone(),
                );

                // Now we add a timed call for 30 seconds from now to indicate via splice in that
                // the stream can go back to normal programming. This is not strictly necessary
                // since we are saying how long our splice out should be, but it is good
                // to have this indication anyway.
                glib::timeout_add(Duration::from_secs(30), {
                    let muxer_weak = muxer.downgrade();
                    let ad_event_counter = ad_event_counter.clone();
                    move || {
                        if let Some(muxer) = muxer_weak.upgrade() {
                            // next event id
                            let event_id =  {
                                let mut ad_event_counter = ad_event_counter.lock().unwrap();
                                *ad_event_counter += 1;
                                *ad_event_counter
                            };
                            let now = muxer.current_running_time().unwrap();
                            send_splice_in(&muxer, event_id, now + ahead + ad_duration);
                        }
                        // This don't need to run again
                        glib::Continue(false)
                    }
                });

            }
            // Run this again next time...
            glib::Continue(true)
        }
    });


    let context = glib::MainContext::default();
    let main_loop = glib::MainLoop::new(Some(&context), false);

    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();
    bus.add_watch({
        let main_loop = main_loop.clone();
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
                _ => (),
            };

            glib::Continue(true)
        }
    })
    .expect("Failed to add bus watch");

    ctrlc::set_handler({
        let pipeline_weak = pipeline.downgrade();
        move || {
            if let Some(pipeline) = pipeline_weak.upgrade() {
                pipeline.call_async(|itself| {

                    let dot_graph = itself
                        .debug_to_dot_data(gst::DebugGraphDetails::all())
                        .to_string();
                    let mut graph = File::create("pipeline.dot").unwrap();
                    graph.write_all(dot_graph.as_bytes()).unwrap();

                    itself.set_state(gst::State::Null).unwrap();

                    process::exit(0);
                });
            }
        }
    })?;

    main_loop.run();
    bus.remove_watch().unwrap();

    pipeline.set_state(gst::State::Null)?;

    Ok(())
}
