use gst::prelude::*;
use gstreamer_app as gst_app;
use log::{debug, error, info, trace, warn};
use std::process;


fn main() -> eyre::Result<()> {
    pretty_env_logger::init_timed();
    gst::init()?;

    gstrstextwrap::plugin_register_static()?;
    gstrusoto::plugin_register_static()?;
    gstrsclosedcaption::plugin_register_static()?;
    gstvosk::plugin_register_static()?;

    // ristsrc name=rist_src address="0.0.0.0" ! rtpmp2tdepay name=rtmpdepay ! decodebin name=multiplexer
    // uridecodebin name=demuxer uri=file:///Users/rafael.caricio/video.mkv
    //
    // demuxer. ! videorate ! video/x-raw,framerate=(fraction)30/1 ! ccextractor remove-caption-meta=true ! transcriberbin name=trans latency=30000
    // demuxer. ! audio/x-raw ! audiorate ! audioconvert ! audioresample ! trans.sink_audio

    let pipeline = gst::parse_launch(
        r#"

        uridecodebin name=multiplexer uri=file:///Users/rafael.caricio/video.mkv

        multiplexer. ! videorate ! video/x-raw,framerate=(fraction)30/1 ! ccextractor remove-caption-meta=true ! trans.sink_video
        multiplexer. ! audio/x-raw ! audiorate ! audioconvert ! audioresample ! transcriberbin name=trans

        trans.src_video ! cea608overlay black-background=1 ! autovideosink
        trans.src_audio ! autoaudiosink

    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();

    info!("Starting pipeline...");

    // let demuxer = pipeline.by_name("demuxer").unwrap();
    // demuxer.connect_pad_added(|_, pad| {
    //     let name = pad.name();
    //     let caps = pad.caps().unwrap();
    //     let caps_type = caps.structure(0).unwrap().name();
    //     info!("Pad {} added with caps {}", name, caps_type);
    // });

    let transcriber = gst::ElementFactory::make("gspeechtotext", None).expect("Could not instantiate Google transcriber");
    transcriber.set_property("auth-json-file",
                             "/Users/rafael.caricio/development/live/google-cloud-playground/i-centralvideo-dictate-dev-c184dd68967a.json");
    let transcriber_bin = pipeline.by_name("trans").expect("Trans bin");
    transcriber_bin.set_property("transcriber", transcriber);
    transcriber_bin.set_property("latency", 45_000_u32);


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
            let pipeline = pipeline_weak.upgrade().unwrap();
            pipeline.set_state(gst::State::Null).unwrap();
        }
    })?;

    main_loop.run();
    bus.remove_watch().unwrap();

    pipeline.set_state(gst::State::Null)?;

    // let mut out_wav_buffer = Cursor::new(Vec::new());
    // let mut writer = WavWriter::new(
    //     &mut out_wav_buffer,
    //     WavSpec {
    //         channels: 1,
    //         sample_rate: 48000,
    //         bits_per_sample: 16,
    //         sample_format: hound::SampleFormat::Int,
    //     },
    // )
    // .unwrap();
    //
    // let mut raw_audio_content = Cursor::new(raw_audio_content.lock().unwrap().to_vec());
    //
    // while let Ok(sample) = raw_audio_content.read_i16::<LittleEndian>() {
    //     writer.write_sample(sample).unwrap();
    // }
    //
    // drop(writer);
    // let mut file = File::create("out.raw")?;
    // file.write_all(&raw_audio_content.into_inner())?;

    Ok(())
}
