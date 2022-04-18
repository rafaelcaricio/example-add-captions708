use glib::translate::ToGlibPtr;
use gst::prelude::*;
use log::{debug, info, trace};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use clap::Parser;
use url::Url;

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

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Configuration {
    /// HLS input source of the stream
    #[clap(short='s', long)]
    hls_source_url: String,

    /// RTP destination for the stream
    #[clap(short='d', long)]
    rtp_destination_url: String,

    /// Image of the frame used as reference to trigger the SCTE35 event
    #[clap(short='i', long)]
    slate_image_path: String,

    /// The SCTE35 stream PID
    #[clap(short='p', long, default_value="500")]
    scte_pid: u32,

    /// The duration of the SCTE35 Splice event in seconds
    #[clap(short='d', long)]
    scte_duration_secs: u64,
}

fn main() -> eyre::Result<()> {
    pretty_env_logger::init_timed();
    gst::init()?;
    gstvideofx::plugin_register_static()?;
    unsafe {
        gst_mpegts::gst_mpegts_initialize();
    }

    let conf: Configuration = Configuration::parse();

    let pipeline = gst::parse_launch(
        r#"

        urisourcebin name=source ! tsdemux name=demux ! queue ! h264parse ! tee name=v

        v. ! queue ! mpegtsmux name=mux scte-35-null-interval=450000 ! rtpmp2tpay ! udpsink name=rtpsink sync=true
        v. ! queue ! decodebin ! videoconvert ! imgcmp name=imgcmp ! autovideosink

        demux. ! queue ! aacparse ! mux.
    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();

    info!("Starting pipeline...");

    // Set the HLS source URL
    let source = pipeline.by_name("source").unwrap();
    source.set_property("uri", conf.hls_source_url);

    // Set SCTE35 stream PID
    let mux = pipeline.by_name("mux").unwrap();
    mux.set_property("scte-35-pid", conf.scte_pid);

    // Set the frame we are searching for
    let imgcmp = pipeline.by_name("imgcmp").unwrap();
    imgcmp.set_property("location", conf.slate_image_path);

    // Set the RTP destination
    let rtp_url = Url::parse(&conf.rtp_destination_url).expect("Valid URL in format rtp://<host>:<port>");
    let rtp_sink = pipeline.by_name("rtpsink").unwrap();
    rtp_sink.set_properties(&[
        ("host", &rtp_url.host_str().unwrap().to_string()),
        ("port", &(rtp_url.port().unwrap() as i32)),
        // In order to make sure the slate image is not event visible, we delay 1 second
        ("ts-offset", &(gst::ClockTime::from_seconds(1).nseconds() as i64)),
    ]);

    let context = glib::MainContext::default();
    let main_loop = glib::MainLoop::new(Some(&context), false);

    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();
    bus.add_watch({
        let event_counter = EventId::default();
        let ad_running = Arc::new(AtomicBool::new(false));
        let ad_duration = Duration::from_secs(conf.scte_duration_secs);

        let main_loop = main_loop.clone();
        let pipeline_weak = pipeline.downgrade();
        let imgcmp_weak = imgcmp.downgrade();
        let muxer_weak = mux.downgrade();
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
                        trace!("Element Message: {:?}", elem_msg);
                        if elem_msg.src().map(|e| e == imgcmp).unwrap_or(false)
                            && elem_msg.message().has_name("image-detected")
                            && !ad_running.load(Ordering::Relaxed)
                        {
                            // We need to notify a specific time in the stream where the SCTE-35 marker
                            // is, so we use the pipeline running time to base our timing calculations
                            let now = pipeline.current_running_time().unwrap();

                            // Trigger the Splice Out event in the SCTE-35 stream
                            send_splice_out(&muxer, event_counter.next(), now);
                            ad_running.store(true, Ordering::Relaxed);
                            info!("Ad started..");

                            // Now we add a timed call for the duration of the ad from now to indicate via
                            // splice in that the stream can go back to normal programming.
                            glib::timeout_add(ad_duration, {
                                let muxer_weak = muxer.downgrade();
                                let event_counter = event_counter.clone();
                                let ad_running = Arc::clone(&ad_running);
                                move || {
                                    if let Some(muxer) = muxer_weak.upgrade() {
                                        let now = muxer.current_running_time().unwrap();
                                        send_splice_in(&muxer, event_counter.next(), now);
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
