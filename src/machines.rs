use std::{
    array,
    ops::Div,
    sync::{Arc, Mutex},
    time::Duration,
};

use itertools::Itertools;
use serde::Serialize;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_serial::SerialPortBuilderExt;

#[derive(Default, PartialEq, Eq, Serialize, Clone, Copy, Debug)]
pub enum PowerStatus {
    #[default]
    NoClue,
    Broken,
    Off,
    On,
}

#[derive(Serialize, Clone, Copy, Debug)]
struct State {
    power_status: PowerStatus,
    #[serde(with = "serde_millis")]
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
    pub scan_duration: Duration,
    pub cutoff: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            forced_state: None,
            scan_duration: Duration::from_secs(60),
            cutoff: 700,
        }
    }
}

#[derive(Serialize)]
pub struct Machines<const T: usize> {
    #[serde(with = "serde_arrays")]
    status: [Arc<Mutex<State>>; T],
}

pub trait Mean: Sized {
    fn mean<A>(self) -> A
    where
        Self: Iterator<Item = A>,
        A: std::iter::Sum<A> + Div<A, Output = A> + Default + TryFrom<usize>,
        <A as TryFrom<usize>>::Error: std::fmt::Debug,
    {
        let i = self.collect_vec();
        let len = i.len().try_into().unwrap();
        i.into_iter()
            .sum1::<A>()
            .map(|v| v / len)
            .unwrap_or_default()
    }
}

impl<T: std::iter::Iterator<Item = u64>> Mean for T {}

fn analyze_values(values: &[f32], cutoff: u64) -> PowerStatus {
    match values
        .into_iter()
        .map(|v| (1000. * v).trunc() as u64)
        .sorted()
        .skip(2)
        .rev()
        .skip(2)
        .mean()
    {
        x if x < cutoff => PowerStatus::Off,
        _ => PowerStatus::On,
    }
}

impl<const T: usize> Machines<T> {
    pub fn new(path: &str, baud_rate: u32, thresholds: [Thresholds; T]) -> Self {
        let serial_port = tokio_serial::new(path, baud_rate)
            .timeout(Duration::from_secs(1))
            .open_native_async()
            .expect(&format!("Cannot Open Serial Port: {}", path));

        let mut reader = BufReader::new(serial_port);

        let status: [Arc<Mutex<State>>; T] =
            std::array::from_fn(|i| Arc::new(Mutex::new(State::new(thresholds[i].forced_state))));

        let thread_status = status.clone();

        tokio::spawn(async move {
            let mut line = String::new();
            let mut values: [Vec<(f32, Instant)>; T] = array::from_fn(|_| Vec::new());
            while let Ok(_) = reader.read_line(&mut line).await {
                if let Ok(one_line) = line
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
                            let Some((_, time_of_first_record)) = v.first() else {
                                return;
                            };

                            if Instant::now().duration_since(*time_of_first_record)
                                >= t.scan_duration
                            {
                                let (values, _): (Vec<_>, Vec<_>) = v.iter().cloned().unzip();

                                let new = analyze_values(&values, t.cutoff);

                                let mut old_value = status.lock().unwrap();

                                if (*old_value).power_status != new {
                                    (*old_value).power_status = new;
                                    (*old_value).last_updated = Instant::now();
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
