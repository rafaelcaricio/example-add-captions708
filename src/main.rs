use gst::prelude::*;
use gstreamer_app as gst_app;
use log::{debug, error, info, trace, warn};
use serde_derive::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Cursor, Write};
use std::process;
use std::sync::{Arc, Mutex};
use tungstenite::{connect, Message};
use url::Url;

#[derive(Deserialize, Serialize, Debug)]
pub struct Configuration {
    config: ConfigInner,
}

#[derive(Deserialize, Serialize, Debug)]
struct ConfigInner {
    /// Sample rate the audio will be provided at.
    sample_rate: i32,

    /// Show time ranges of each word in the transcription.
    words: bool,
}

impl Configuration {
    pub fn new(sample_rate: i32) -> Self {
        Self {
            config: ConfigInner {
                sample_rate,
                // We always want to receive the words with their time ranges.
                words: true,
            },
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Transcript {
    pub result: Vec<WordInfo>,
    pub text: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct WordInfo {
    #[serde(rename = "conf")]
    pub confidence: f64,
    pub word: String,
    pub start: f64,
    pub end: f64,
}

fn main() -> eyre::Result<()> {
    pretty_env_logger::init_timed();
    gst::init()?;

    let pipeline = gst::parse_launch(
        r#"
        uridecodebin uri=file:///Users/rafael.caricio/video.mkv name=dec dec.src_1 ! audio/x-raw !
        audioconvert ! audiorate ! audioresample ! audio/x-raw,format=S16LE,rate=16000,channels=1 ! appsink name=sink
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

    let (mut socket, response) = connect(Url::parse("ws://localhost:2700").unwrap())?;

    let config = Configuration::new(16_000);
    info!(
        "config payload: {}",
        serde_json::to_string_pretty(&config).unwrap()
    );
    let packet = serde_json::to_string(&config).unwrap();
    socket.write_message(Message::Text(packet)).unwrap();

    let shared_socket = Arc::new(Mutex::new(socket));

    let app_sink = pipeline
        .by_name("sink")
        .unwrap()
        .downcast::<gst_app::AppSink>()
        .unwrap();
    app_sink.set_sync(false);
    app_sink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample({
                let shared_socket = shared_socket.clone();
                move |app| {
                    let sample = app.pull_sample().unwrap();
                    let buffer = sample.buffer().unwrap();
                    let data = buffer.map_readable().unwrap();

                    let mut socket = shared_socket.lock().unwrap();

                    for chunk in data.chunks(8_000) {
                        socket
                            .write_message(Message::Binary(chunk.to_vec()))
                            .unwrap();

                        let msg = socket.read_message().unwrap();
                        match msg {
                            Message::Text(payload) => {
                                match serde_json::from_str::<Transcript>(&payload) {
                                    Ok(transcript) => {
                                        let text = transcript
                                            .result
                                            .iter()
                                            .map(|p| p.word.to_string())
                                            .collect::<Vec<_>>()
                                            .join(" ");
                                        info!("result: {}", text);
                                    }
                                    Err(_) => {
                                        // The payload is still not a final transcript, so we just ignore it
                                        // info!("No results...");
                                    }
                                }
                            }
                            _ => {}
                        };
                    }

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
