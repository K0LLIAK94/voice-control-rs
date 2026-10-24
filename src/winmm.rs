//! WinMM input reconstructed from Laitis's NanoAudio WaveInEvent configuration:
//! 16 kHz PCM, 16-bit mono, four 40 ms buffers. No target code is executed.

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Receiver, Sender, bounded};
use std::{
    cell::UnsafeCell,
    ffi::c_void,
    mem::size_of,
    pin::Pin,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const SAMPLE_RATE: u32 = 16_000;
const SAMPLES: usize = 640;
const BUFFER_BYTES: u32 = (SAMPLES * size_of::<i16>()) as u32;
const BUFFER_COUNT: usize = 4;
const WAVE_MAPPER: u32 = u32::MAX;
const CALLBACK_EVENT: u32 = 0x0005_0000;
const WHDR_DONE: u32 = 1;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;

type Handle = *mut c_void;

#[repr(C, packed(1))]
struct WaveFormatEx {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    extra_size: u16,
}

#[repr(C)]
struct WaveHdr {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHdr,
    reserved: usize,
}

#[repr(C)]
struct WaveInCapsW {
    manufacturer_id: u16,
    product_id: u16,
    driver_version: u32,
    product_name: [u16; 32],
    formats: u32,
    channels: u16,
    reserved: u16,
}

#[link(name = "winmm")]
unsafe extern "system" {
    fn waveInGetNumDevs() -> u32;
    fn waveInGetDevCapsW(device: usize, caps: *mut WaveInCapsW, size: u32) -> u32;
    fn waveInGetErrorTextW(error: u32, text: *mut u16, size: u32) -> u32;
    fn waveInGetID(input: Handle, device: *mut u32) -> u32;
    fn waveInOpen(
        input: *mut Handle,
        device: u32,
        format: *const WaveFormatEx,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveInPrepareHeader(input: Handle, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInUnprepareHeader(input: Handle, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInAddBuffer(input: Handle, header: *mut WaveHdr, size: u32) -> u32;
    fn waveInStart(input: Handle) -> u32;
    fn waveInStop(input: Handle) -> u32;
    fn waveInReset(input: Handle) -> u32;
    fn waveInClose(input: Handle) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(
        attributes: *const c_void,
        manual_reset: i32,
        initial_state: i32,
        name: *const u16,
    ) -> Handle;
    fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
    fn CloseHandle(handle: Handle) -> i32;
}

fn mm_error(operation: &str, code: u32) -> anyhow::Error {
    let mut text = [0_u16; 256];
    // SAFETY: text is writable storage with the supplied character count.
    let result = unsafe { waveInGetErrorTextW(code, text.as_mut_ptr(), text.len() as u32) };
    let detail = if result == 0 {
        let length = text
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(text.len());
        String::from_utf16_lossy(&text[..length])
    } else {
        "неизвестная ошибка WinMM".into()
    };
    anyhow::anyhow!("{operation}: {detail} (WinMM {code})")
}

fn checked(operation: &str, result: u32) -> Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(mm_error(operation, result))
    }
}

fn device_name(device: u32) -> Result<String> {
    // SAFETY: WaveInCapsW has the native WAVEINCAPSW layout and no references.
    let mut caps: WaveInCapsW = unsafe { std::mem::zeroed() };
    checked("Имя микрофона", unsafe {
        waveInGetDevCapsW(device as usize, &mut caps, size_of::<WaveInCapsW>() as u32)
    })?;
    let length = caps
        .product_name
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(caps.product_name.len());
    Ok(String::from_utf16_lossy(&caps.product_name[..length]))
}

pub fn list() -> Result<Vec<String>> {
    // SAFETY: this only reads the native device count.
    let count = unsafe { waveInGetNumDevs() };
    (0..count).map(device_name).collect()
}

struct Buffer {
    // UnsafeCell permits the driver to write through its retained raw pointers.
    // Both allocations remain fixed until reset and unprepare have succeeded.
    header: Pin<Box<UnsafeCell<WaveHdr>>>,
    samples: Box<UnsafeCell<[i16; SAMPLES]>>,
    prepared: bool,
}

impl Buffer {
    fn new() -> Self {
        let samples = Box::new(UnsafeCell::new([0; SAMPLES]));
        let header = Box::pin(UnsafeCell::new(WaveHdr {
            data: samples.get().cast::<u8>(),
            buffer_length: BUFFER_BYTES,
            bytes_recorded: 0,
            user: 0,
            flags: 0,
            loops: 0,
            next: ptr::null_mut(),
            reserved: 0,
        }));
        Self {
            header,
            samples,
            prepared: false,
        }
    }

    fn header_ptr(&self) -> *mut WaveHdr {
        self.header.as_ref().get_ref().get()
    }

    fn completed(&self) -> bool {
        // SAFETY: the header allocation is pinned; volatile reads observe the
        // driver's WHDR_DONE ownership handoff without creating a Rust borrow.
        unsafe { ptr::read_volatile(ptr::addr_of!((*self.header_ptr()).flags)) & WHDR_DONE != 0 }
    }

    fn copy_samples(&self) -> Result<Vec<i16>> {
        // SAFETY: called only after WHDR_DONE and before this buffer is requeued.
        let length =
            unsafe { ptr::read_volatile(ptr::addr_of!((*self.header_ptr()).bytes_recorded)) };
        if length > BUFFER_BYTES || length % 2 != 0 {
            bail!("Микрофон вернул некорректный размер буфера: {length}");
        }
        Ok(unsafe {
            std::slice::from_raw_parts(self.samples.get().cast::<i16>(), length as usize / 2)
        }
        .to_vec())
    }
}

struct Capture {
    input: Handle,
    event: Handle,
    buffers: Vec<Buffer>,
    name: String,
}

impl Capture {
    fn open(name: &str) -> Result<Self> {
        let device = if name.is_empty() {
            WAVE_MAPPER
        } else {
            let names = list()?;
            names
                .iter()
                .position(|candidate| candidate == name)
                .with_context(|| {
                    format!("Микрофон «{name}» не найден. Проверьте список устройств и config.toml")
                })? as u32
        };
        // SAFETY: creates an unnamed auto-reset event owned by this capture.
        let event = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if event.is_null() {
            return Err(std::io::Error::last_os_error()).context("Событие микрофона Windows");
        }
        let mut capture = Self {
            input: ptr::null_mut(),
            event,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            name: String::new(),
        };
        let format = WaveFormatEx {
            format_tag: 1,
            channels: 1,
            samples_per_sec: SAMPLE_RATE,
            avg_bytes_per_sec: SAMPLE_RATE * 2,
            block_align: 2,
            bits_per_sample: 16,
            extra_size: 0,
        };
        // SAFETY: format lives for the call. CALLBACK_EVENT is an owned kernel
        // event, not a function pointer; driver notifications cannot call Rust.
        checked(
            "Не удалось открыть микрофон (разрешите доступ в Windows)",
            unsafe {
                waveInOpen(
                    &mut capture.input,
                    device,
                    &format,
                    capture.event as usize,
                    0,
                    CALLBACK_EVENT,
                )
            },
        )?;
        let mut actual_device = device;
        let id_result = unsafe { waveInGetID(capture.input, &mut actual_device) };
        capture.name = if id_result == 0 {
            device_name(actual_device)?
        } else if name.is_empty() {
            "Микрофон Windows по умолчанию".into()
        } else {
            name.into()
        };
        for _ in 0..BUFFER_COUNT {
            capture.buffers.push(Buffer::new());
            let buffer = capture.buffers.last_mut().expect("buffer was just pushed");
            checked("Подготовка буфера микрофона", unsafe {
                waveInPrepareHeader(
                    capture.input,
                    buffer.header_ptr(),
                    size_of::<WaveHdr>() as u32,
                )
            })?;
            buffer.prepared = true;
            checked("Очередь буфера микрофона", unsafe {
                waveInAddBuffer(
                    capture.input,
                    buffer.header_ptr(),
                    size_of::<WaveHdr>() as u32,
                )
            })?;
        }
        checked("Запуск микрофона", unsafe {
            waveInStart(capture.input)
        })?;
        Ok(capture)
    }

    fn run(
        &mut self,
        stop: &AtomicBool,
        chunks: &Sender<Vec<i16>>,
        level: &AtomicU32,
        dropped: &AtomicU64,
    ) -> Result<()> {
        let mut next = 0;
        while !stop.load(Ordering::Acquire) {
            // SAFETY: the capture owns event until the worker exits.
            let wait = unsafe { WaitForSingleObject(self.event, 200) };
            if wait != WAIT_OBJECT_0 && wait != WAIT_TIMEOUT {
                return Err(std::io::Error::last_os_error()).context("Ожидание данных микрофона");
            }
            if stop.load(Ordering::Acquire) {
                break;
            }
            // Preserve capture order even when several buffers complete before
            // the worker wakes and the ring wraps around index zero.
            for _ in 0..BUFFER_COUNT {
                let buffer = &self.buffers[next];
                if !buffer.completed() {
                    break;
                }
                let pcm = buffer.copy_samples()?;
                let peak = pcm
                    .iter()
                    .map(|sample| u32::from(sample.unsigned_abs()))
                    .max()
                    .unwrap_or(0);
                level.store((peak * 100 / 32768).min(100), Ordering::Relaxed);
                if !pcm.is_empty() && chunks.try_send(pcm).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
                checked("Возврат буфера микрофону", unsafe {
                    waveInAddBuffer(self.input, buffer.header_ptr(), size_of::<WaveHdr>() as u32)
                })?;
                next = (next + 1) % BUFFER_COUNT;
            }
        }
        level.store(0, Ordering::Relaxed);
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if !self.input.is_null() {
            // SAFETY: only this worker calls WinMM. Reset returns all queued
            // buffers before they are unprepared and their allocations freed.
            unsafe {
                waveInStop(self.input);
                waveInReset(self.input);
            }
            for _ in 0..5 {
                for buffer in &mut self.buffers {
                    if buffer.prepared
                        && unsafe {
                            waveInUnprepareHeader(
                                self.input,
                                buffer.header_ptr(),
                                size_of::<WaveHdr>() as u32,
                            )
                        } == 0
                    {
                        buffer.prepared = false;
                    }
                }
                if self.buffers.iter().all(|buffer| !buffer.prepared) {
                    break;
                }
                unsafe {
                    waveInReset(self.input);
                }
                thread::sleep(Duration::from_millis(20));
            }
            let released = self.buffers.iter().all(|buffer| !buffer.prepared)
                && unsafe { waveInClose(self.input) } == 0;
            if !released {
                // A broken driver must not get dangling header/PCM/event
                // pointers. Leak only this failed native capture rather than
                // freeing memory that Windows may still own.
                for buffer in self.buffers.drain(..) {
                    std::mem::forget(buffer);
                }
                self.event = ptr::null_mut();
            }
            self.input = ptr::null_mut();
        }
        if !self.event.is_null() {
            // SAFETY: the waveform handle is closed or was never opened.
            unsafe {
                CloseHandle(self.event);
            }
            self.event = ptr::null_mut();
        }
    }
}

struct Worker {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

pub struct Microphone {
    _internal: Worker,
    pub chunks: Receiver<Vec<i16>>,
    pub errors: Receiver<String>,
    pub level: Arc<AtomicU32>,
    pub dropped: Arc<AtomicU64>,
    pub description: String,
}

impl Microphone {
    pub fn open(name: &str) -> Result<Self> {
        let (chunks_tx, chunks) = bounded(BUFFER_COUNT);
        let (errors_tx, errors) = bounded(4);
        let (ready_tx, ready_rx) = bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let level = Arc::new(AtomicU32::new(0));
        let dropped = Arc::new(AtomicU64::new(0));
        let worker_stop = stop.clone();
        let worker_level = level.clone();
        let worker_dropped = dropped.clone();
        let name = name.to_owned();
        let worker = thread::Builder::new()
            .name("winmm-microphone".into())
            .spawn(move || {
                let mut capture = match Capture::open(&name) {
                    Ok(capture) => capture,
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("{error:#}")));
                        return;
                    }
                };
                let description = format!(
                    "{} · WinMM · 16000 Гц · 16 бит · моно · 4 × 40 мс",
                    capture.name
                );
                if ready_tx.send(Ok(description)).is_err() {
                    return;
                }
                if let Err(error) =
                    capture.run(&worker_stop, &chunks_tx, &worker_level, &worker_dropped)
                {
                    let _ = errors_tx.try_send(format!("{error:#}"));
                }
            })
            .context("Поток микрофона Windows")?;
        let internal = Worker {
            stop,
            thread: Some(worker),
        };
        let description = ready_rx
            .recv()
            .context("Поток микрофона завершился при запуске")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            _internal: internal,
            chunks,
            errors,
            level,
            dropped,
            description,
        })
    }
}
