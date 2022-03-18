use gst::prelude::*;
use gstreamer_app as gst_app;
use log::{debug, error, info, trace, warn};
use std::process;

fn main() -> eyre::Result<()> {
    pretty_env_logger::init();
    gst::init()?;
    gstrusoto::plugin_register_static()?;
    gstrsclosedcaption::plugin_register_static()?;
    gstrstextwrap::plugin_register_static()?;

    let pipeline = gst::parse_launch(
        r#"
        souphttpsrc location="https://playertest.longtailvideo.com/adaptive/elephants_dream_v4/redundant.m3u8" ! hlsdemux name=demuxer

        demuxer.src_0 ! decodebin ! cccombiner name=ccc_fr ! videoconvert ! x264enc ! video/x-h264,profile=main ! muxer.video_0
        demuxer.src_1 ! decodebin ! audioconvert ! audioresample ! opusenc ! audio/x-opus,rate=48000,channels=2 ! muxer.audio_0
        demuxer.src_2 ! decodebin ! audioconvert ! audioresample ! opusenc ! audio/x-opus,rate=48000,channels=2 ! muxer.audio_1
        demuxer.src_3 ! decodebin ! audioconvert ! audioresample ! opusenc ! audio/x-opus,rate=48000,channels=2 ! muxer.audio_2

        souphttpsrc location="https://playertest.longtailvideo.com/adaptive/elephants_dream_v4/media_b/french/ed.m3u8" ! hlsdemux ! subparse ! tttocea608 ! ccconverter ! closedcaption/x-cea-708,format=cc_data ! ccc_fr.caption

        qtmux name=muxer ! filesink location=output_cae708_only_fr.mp4
    "#,
    )?
        .downcast::<gst::Pipeline>()
        .unwrap();
    pipeline.set_async_handling(true);

    // souphttpsrc location="https://playertest.longtailvideo.com/adaptive/elephants_dream_v4/media_b/chinese/ed.m3u8" ! hlsdemux ! subparse ! tttocea608 ! ccconverter ! closedcaption/x-cea-708,format=cc_data ! ccc_ch.caption
    // souphttpsrc location="https://playertest.longtailvideo.com/adaptive/elephants_dream_v4/media_b/french/ed.m3u8" ! hlsdemux ! subparse ! tttocea608 ! appsink name=sink

    info!("Starting pipeline...");

    let demuxer = pipeline.by_name("demuxer").unwrap();
    demuxer.connect_pad_added(|_, pad| {
        let name = pad.name();
        let caps = pad.caps().unwrap();
        let caps_type = caps.structure(0).unwrap().name();
        // dbg!(name);
        debug!("Pad {} added with caps {}", name, caps_type);
    });
    // let app_sink = pipeline
    //     .by_name("sink")
    //     .unwrap()
    //     .downcast::<gst_app::AppSink>()
    //     .unwrap();
    // app_sink.set_sync(false);
    // app_sink.set_callbacks(
    //     gst_app::AppSinkCallbacks::builder()
    //         .new_sample(move |app| {
    //             let sample = app.pull_sample().unwrap();
    //             let buffer = sample.buffer().unwrap();
    //
    //             // We don't care about buffers that are not video
    //             if buffer
    //                 .flags()
    //                 .contains(gst::BufferFlags::DECODE_ONLY | gst::BufferFlags::GAP)
    //             {
    //                 return Ok(gst::FlowSuccess::Ok);
    //             }
    //
    //             // let data = buffer.map_readable().unwrap();
    //             // let text = std::str::from_utf8(&data).unwrap();
    //             // println!("Subtext = {}", text);
    //             dbg!(buffer);
    //
    //             Ok(gst::FlowSuccess::Ok)
    //         })
    //         .build(),
    // );

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

    Ok(())
}
