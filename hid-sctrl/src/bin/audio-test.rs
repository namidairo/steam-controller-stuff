use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use bytes::{Buf, Bytes};
use clap::Parser;
use eyre::Context;
use hidapi::HidApi;
use hid_sctrl::haptics::{HapticsStreamer, StreamStatus};
use hid_sctrl::io::{hid_get_input_report, hid_set_output_report};
use tracing::{debug, info, trace, warn};

#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(clap::Subcommand)]
enum Commands {
    TestAudio(TestAudioArgs),
}

#[derive(clap::Args)]
struct TestAudioArgs {
    /// audio file
    audio_file: PathBuf,
}

fn main() -> eyre::Result<ExitCode> {
    hid_sctrl::common::initialize_logging();
    info!("Hello World");

    let args = Args::parse();
    match args.command {
        Commands::TestAudio(args) => test_audio(args),
    }
}

const HID_REPORT_SIZE: usize = 64;

fn test_audio(args: TestAudioArgs) -> eyre::Result<ExitCode> {
    // target tick rate in ms
    const TICKRATE: u64 = 1;
    const TICKRATE_DURATION: Duration = Duration::from_millis(TICKRATE);

    let mut wake_time = Instant::now();

    let api = HidApi::new().wrap_err("opening hidapi instance")?;
    

    let mut hid_device: hidapi::HidDevice = api
        .open(0x28DE, 0x1302)
        .wrap_err("opening hid device failed")?;

    let mut in_buf = [0u8; HID_REPORT_SIZE];
    let mut out_buf = [0u8; HID_REPORT_SIZE];

    let mut tick_counter: u64 = 0;

    let (left_send, left_recv) = async_channel::bounded::<Bytes>(64);
    let (right_send, right_recv) = async_channel::bounded::<Bytes>(64);

    let bytes_per_sample = 2;
    let samples_per_ms = 8;
    let baseline_rate = bytes_per_sample * samples_per_ms;
    let mut streamers = [
        HapticsStreamer::new(0, left_recv, TICKRATE, baseline_rate), // INT_LEFT
        HapticsStreamer::new(4, right_recv, TICKRATE, baseline_rate), // INT_RIGHT
                                                                     // HapticsStreamer::new(2, left_recv, TICKRATE, baseline_rate), // INT_BOTH
    ];

    let mut in_file = std::fs::File::open(&args.audio_file).wrap_err("opening pcm file")?;

    std::thread::spawn(move || {
        fn transform_sample(sample: i16) -> i16 {
            (sample as f32 * 0.9) as i16
        }

        let mut buf = [0u8; 65536];
        loop {
            let bytes_read = in_file.read(&mut buf).expect("reading pcm file");
            if bytes_read == 0 {
                // g'bye
                return;
            }

            let read_slice = &buf[..bytes_read];

            // TOOD: actually handle partial reads
            // heheheh shitcode
            assert!(bytes_read.is_multiple_of(2), "oops");

            let mut left_buf = Vec::with_capacity(bytes_read / 2);
            let mut right_buf = Vec::with_capacity(bytes_read / 2);

            for mut chunk in read_slice.chunks_exact(4) {
                let left_sample = chunk.get_i16_le();
                let right_sample = chunk.get_i16_le();

                left_buf.extend(transform_sample(left_sample).to_le_bytes());
                right_buf.extend(transform_sample(right_sample).to_le_bytes());
            }

            left_send
                .send_blocking(Bytes::from_owner(left_buf))
                .expect("receiver gone");
            right_send
                .send_blocking(Bytes::from_owner(right_buf))
                .expect("receiver gone");
        }
    });

    // configure for 8khz s8 pcm, both INT_LEFT and INT_RIGHT
    hid_set_output_report(&mut hid_device, &[0x86, 0x02, 0x02, 0x00]).wrap_err("configuring stream")?;

    // HACK: wait for reader to actually read some data and also for the controller to process
    //       the configuration request.
    //       ideally we'd wait for ack before starting. TODO.
    std::thread::sleep(Duration::from_millis(200));

    let mut next_streamer = 0;

    loop {
        let loop_start = Instant::now();
        let error = loop_start.saturating_duration_since(wake_time);
        if error > TICKRATE_DURATION {
            // joever
            warn!("ran out of time, resetting! {error:?} behind");
            wake_time = Instant::now();
        }
        wake_time += TICKRATE_DURATION;

        'handle_report: {
            let Some(in_report) =
                hid_get_input_report(&mut hid_device, &mut in_buf).wrap_err("reading input report")?
            else {
                break 'handle_report;
            };

            if in_report.is_empty() {
                warn!("input report too short");
                break 'handle_report;
            }

            match in_report[0] {
                0x44 => {
                    if in_report.len() < 3 {
                        warn!("stream feedback report too short");
                        break 'handle_report;
                    }

                    let target = in_report[1];
                    let status = StreamStatus::from_bits_truncate(in_report[2]);

                    match target {
                        0 => streamers[0].handle_status(status),
                        1 => streamers[1].handle_status(status),
                        n => debug!("ignoring unknown target {n}"),
                    }
                }
                id => trace!("ignoring unhandled input report 0x{id:x}"),
            }
        }

        for s in &mut streamers {
            s.tick();
        }

        // TODO: less shit code?
        for _ in 0..streamers.len() {
            let len = streamers[next_streamer].poll_send(&mut out_buf);
            if len > 0 {
                debug!("sending buffer for {}", streamers[next_streamer].id);
                hid_set_output_report(&mut hid_device, &out_buf[..len])
                    .wrap_err("writing output report")?;
            }
            next_streamer = (next_streamer + 1) % streamers.len();
            if len > 0 {
                break;
            }
        }

        if streamers.iter().all(|s| s.ended) {
            info!("done");
            return Ok(ExitCode::SUCCESS);
        }

        {
            // pointless code
            if tick_counter & 0b1 > 0 {
                debug!("tock: {tick_counter}");
            } else {
                debug!("tick: {tick_counter}");
            }
            tick_counter += TICKRATE;
        }

        if let Some(delay) = wake_time.checked_duration_since(Instant::now()) {
            std::thread::sleep(delay);
        }
    }
}
