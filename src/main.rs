use byteorder::{LittleEndian, ReadBytesExt};
use gst::prelude::*;
use gstreamer_app as gst_app;
use hound::{WavSpec, WavWriter};
use log::{debug, error, info, trace, warn};
use std::fs::File;
use std::io::{Cursor, Write};
use std::process;
use std::sync::{Arc, Mutex};

fn main() -> eyre::Result<()> {
    pretty_env_logger::init();
    gst::init()?;

    let pipeline = gst::parse_launch(
        r#"
        uridecodebin uri=file:///Users/rafael.caricio/video.mkv name=dec dec.src_1 ! audio/x-raw !
        audioconvert ! audiorate ! audioresample ! audio/x-raw,format=S16LE,rate=48000,channels=1 ! appsink name=sink
    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();

    info!("Starting pipeline...");

    let demuxer = pipeline.by_name("dec").unwrap();
    demuxer.connect_pad_added(|_, pad| {
        let name = pad.name();
        let caps = pad.caps().unwrap();
        let caps_type = caps.structure(0).unwrap().name();
        debug!("Pad {} added with caps {}", name, caps_type);
    });

    let raw_audio_content = Arc::new(Mutex::new(Vec::new()));

    let app_sink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    app_sink.set_sync(false);
    app_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample({
                let raw_audio_content = raw_audio_content.clone();
                move |app| {
                    let sample = app.pull_sample().unwrap();
                    let buffer = sample.buffer().unwrap();

                    let data = buffer.map_readable().unwrap();

                    let mut raw_audio_content = raw_audio_content.lock().unwrap();
                    raw_audio_content.extend(data.to_vec());

                    Ok(gst::FlowSuccess::Ok)
                }
            })
            .build(),
    );

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

    let mut out_wav_buffer = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(
        &mut out_wav_buffer,
        WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();

    let mut raw_audio_content = Cursor::new(raw_audio_content.lock().unwrap().to_vec());

    while let Ok(sample) = raw_audio_content.read_i16::<LittleEndian>() {
        writer.write_sample(sample).unwrap();
    }

    drop(writer);
    let mut file = File::create("out.wav")?;
    file.write_all(&out_wav_buffer.into_inner())?;

    Ok(())
}
