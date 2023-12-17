use std::{
    array,
    sync::{Arc, Mutex},
    time::Duration,
};

use itertools::Itertools;
use serde::{Serialize, Serializer};
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

fn serialize_last_updated<S>(last_updated: &Instant, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serde_millis::serialize(&Instant::now().duration_since(*last_updated), serializer)
}

#[derive(Serialize, Clone, Copy, Debug)]
struct State {
    power_status: PowerStatus,
    #[serde(serialize_with = "serialize_last_updated")]
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

#[derive(Serialize)]
pub struct Machines<const T: usize> {
    #[serde(with = "serde_arrays")]
    status: [Arc<Mutex<State>>; T],
}

fn analyze_values(values: &[f32], cutoff: u64) -> PowerStatus {
    let values = values
        .iter()
        .map(|v| (1000. * v).trunc() as u64)
        .sorted()
        .collect_vec();

    let median = values[values.len() / 2];

    match median {
        x if x < cutoff => PowerStatus::Off,
        _ => PowerStatus::On,
    }
}

impl<const T: usize> Machines<T> {
    pub fn new(path: &str, baud_rate: u32, thresholds: [Thresholds; T]) -> Self {
        let mut serial_port = tokio_serial::new(path, baud_rate)
            .timeout(Duration::from_secs(1))
            .open_native_async()
            .unwrap_or_else(|_| panic!("Cannot Open Serial Port: {}", path));

        serial_port.set_exclusive(false).unwrap();

        let mut reader = BufReader::new(serial_port);

        let status: [Arc<Mutex<State>>; T] =
            std::array::from_fn(|i| Arc::new(Mutex::new(State::new(thresholds[i].forced_state))));

        let thread_status = status.clone();

        tokio::spawn(async move {
            let mut line = String::new();
            let mut values: [Vec<(f32, Instant)>; T] = array::from_fn(|_| Vec::new());
            while reader.read_line(&mut line).await.is_ok() {
                if let Ok(one_line) = line
                    .trim()
                    .split(' ')
                    .filter_map(|v| v.parse::<f32>().ok())
                    .collect::<Vec<_>>()
                    .try_into()
                {
                    values
                        .iter_mut()
                        .zip::<[f32; T]>(one_line)
                        .for_each(|(a, v)| a.push((v, Instant::now())));

                    values
                        .iter_mut()
                        .zip(thresholds.iter())
                        .zip(thread_status.iter())
                        .for_each(|((v, t), status)| {
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
                                }

                                v.clear();
                            }
                        });
                }

                line = String::new();
            }
        });

        Self { status }
    }
}
