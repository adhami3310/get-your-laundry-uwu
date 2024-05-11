use std::{
    array,
    sync::{Arc, Mutex},
    time::Duration,
};

use itertools::{Either, Itertools};
use serde::Serialize;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_serial::SerialPortBuilderExt;

#[allow(dead_code)]
#[derive(Default, PartialEq, Eq, Serialize, Clone, Copy, Debug)]
pub enum PowerStatus {
    #[default]
    Unknown,
    Broken,
    Off,
    On,
}

#[derive(Serialize, Clone, Copy, Debug)]
struct State {
    power_status: PowerStatus,
    #[serde(serialize_with = "crate::serialize_instant_to_duration_since")]
    #[serde(rename = "since_updated")]
    last_updated: Instant,
}

impl Default for State {
    fn default() -> Self {
        Self {
            power_status: Default::default(),
            last_updated: Instant::now(),
        }
    }
}

impl State {
    pub fn new(forced_state: Option<PowerStatus>) -> Self {
        Self {
            power_status: forced_state.unwrap_or_default(),
            ..Default::default()
        }
    }
}

pub struct Thresholds {
    pub forced_state: Option<PowerStatus>,
    pub off_scan_duration: Duration,
    pub on_scan_duration: Duration,
    pub cutoff: u64,
}

impl Thresholds {
    pub fn with_status(status: PowerStatus) -> Self {
        Self {
            forced_state: Some(status),
            ..Default::default()
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            forced_state: None,
            off_scan_duration: Duration::from_secs(30),
            on_scan_duration: Duration::from_secs(60 * 5),
            cutoff: 700,
        }
    }
}

#[derive(Serialize, Clone)]
pub struct EmailRequest {
    pub kerb: String,
    pub indicies: Vec<usize>,
}

#[derive(Serialize)]
pub struct Timestamp(
    #[serde(serialize_with = "crate::serialize_instant_to_duration_since")] Instant,
);

#[derive(Serialize)]
pub struct Machines<const T: usize> {
    name: &'static str,

    #[serde(with = "serde_arrays")]
    status: [Arc<Mutex<State>>; T],

    #[serde(with = "serde_arrays")]
    latest_values: [Arc<Mutex<f32>>; T],

    #[serde(rename = "since_updated")]
    time_stamp: Arc<Mutex<Timestamp>>,

    email_requests: Arc<Mutex<Vec<EmailRequest>>>,
}

fn analyze_values(values: &[f32], cutoff: u64) -> PowerStatus {
    let values = values
        .iter()
        .map(|v| (1000. * v).trunc() as u64)
        .sorted()
        .collect_vec();

    let large_median = values[values.len() - 1 - (values.len() / 4)];

    match large_median {
        x if x < cutoff => PowerStatus::Off,
        _ => PowerStatus::On,
    }
}

impl<const T: usize> Machines<T> {
    pub fn new(
        name: &'static str,
        path: &str,
        baud_rate: u32,
        thresholds: [Thresholds; T],
    ) -> Self {
        let serial_port = {
            let mut serial_port = loop {
                match tokio_serial::new(path, baud_rate)
                    .timeout(Duration::from_secs(5))
                    .open_native_async()
                {
                    Ok(s) => break s,
                    _ => continue,
                }
            };

            serial_port.set_exclusive(false).unwrap();

            serial_port
        };

        println!("Aquired {path}");

        let mut reader = BufReader::new(serial_port);

        let status: [Arc<Mutex<State>>; T] =
            std::array::from_fn(|i| Arc::new(Mutex::new(State::new(thresholds[i].forced_state))));

        let thread_status = status.clone();

        let email_requests = Arc::new(Mutex::new(Vec::<EmailRequest>::new()));
        let thread_email_requests = email_requests.clone();

        let latest_values: [Arc<Mutex<f32>>; T] = array::from_fn(|_| Default::default());
        let thread_latest_values = latest_values.clone();

        let time_stamp = Arc::new(Mutex::new(Timestamp(Instant::now())));
        let thread_time_stamp = time_stamp.clone();

        tokio::spawn(async move {
            let mut line = String::new();
            let mut values: [Vec<(f32, Instant)>; T] = array::from_fn(|_| Vec::new());
            while reader.read_line(&mut line).await.is_ok() {
                if let Ok(one_line) = TryInto::<[f32; T]>::try_into(
                    line.trim()
                        .split(' ')
                        .filter_map(|v| v.parse::<f32>().ok())
                        .collect::<Vec<_>>(),
                ) {
                    one_line
                        .iter()
                        .zip(&thread_latest_values)
                        .for_each(|(v, t)| {
                            *t.lock().unwrap() = *v;
                        });

                    *thread_time_stamp.lock().unwrap() = Timestamp(Instant::now());

                    values
                        .iter_mut()
                        .zip(one_line)
                        .for_each(|(a, v)| a.push((v, Instant::now())));

                    values
                        .iter_mut()
                        .zip(thresholds.iter())
                        .zip(thread_status.iter())
                        .enumerate()
                        .for_each(|(i, ((v, t), status))| {
                            if t.forced_state.is_some() {
                                v.clear();
                                return;
                            }

                            let Some((_, time_of_first_record)) = v.first() else {
                                return;
                            };

                            let old_state = {
                                let status = status.lock().unwrap();
                                status.power_status
                            };

                            if Instant::now().duration_since(*time_of_first_record)
                                >= match old_state {
                                    PowerStatus::On => t.on_scan_duration,
                                    _ => t.off_scan_duration,
                                }
                            {
                                let (values, _): (Vec<_>, Vec<_>) = v.iter().cloned().unzip();

                                let new = analyze_values(&values, t.cutoff);

                                let mut old_value = status.lock().unwrap();

                                if old_value.power_status != new {
                                    old_value.power_status = new;
                                    old_value.last_updated = Instant::now();

                                    let mut requests = thread_email_requests.lock().unwrap();

                                    let (to_send, new_requests): (Vec<_>, Vec<_>) =
                                        requests.iter().partition_map(|r| {
                                            if r.indicies.contains(&i) {
                                                Either::Left(r.kerb.clone())
                                            } else {
                                                Either::Right(r.to_owned())
                                            }
                                        });

                                    let email_builder = lettre::Message::builder()
                                        .from("Laundry <laundry@mit.edu>".parse().unwrap())
                                        .subject(format!(
                                            "{name} is {} EOM",
                                            match new {
                                                PowerStatus::Broken => "broken",
                                                PowerStatus::Off => "off",
                                                PowerStatus::On => "on",
                                                PowerStatus::Unknown => "unknown",
                                            }
                                        ))
                                        .header(lettre::message::header::ContentType::TEXT_PLAIN);

                                    // Open a remote connection to mit.edu

                                    let mailer = lettre::SmtpTransport::builder_dangerous(
                                        "outgoing.mit.edu",
                                    )
                                    .build();

                                    to_send
                                        .into_iter()
                                        .flat_map(|kerb| lettre::Address::new(kerb, "mit.edu"))
                                        .flat_map(|addr| {
                                            email_builder
                                                .clone()
                                                .to(lettre::message::Mailbox::new(None, addr))
                                                .body(lettre::message::Body::new(vec![]))
                                        })
                                        .for_each(|email| {
                                            // Send the email
                                            if let Err(e) = lettre::Transport::send(&mailer, &email)
                                            {
                                                println!("{}", e);
                                            }
                                        });

                                    *requests = new_requests;
                                }

                                v.clear();
                            }
                        });
                }

                line = String::new();
            }
        });

        Self {
            name,
            status,
            email_requests,
            latest_values,
            time_stamp,
        }
    }

    pub fn add_request(&self, request: EmailRequest) {
        if request.indicies.is_empty() {
            return;
        }
        let mut email_request = self.email_requests.lock().unwrap();
        email_request.push(request);
    }
}
