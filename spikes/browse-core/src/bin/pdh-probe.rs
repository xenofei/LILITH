//! S0.1 (CORE-059): PDH's `GPU Adapter Memory` and `GPU Engine` counters from a process that
//! isn't elevated.
//!
//! `pdh-probe read --out <json>`: adds the three counters (English names), times 300 collects,
//! records the instance-name grammar and the verdict (Step 2).
//! `pdh-probe load --out <json>`: the recorded load part (Step 3, owner says go): 1 GiB of D3D11
//! `STAGING` buffers held and released, then a 30 s compute loop against NVML.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use browse_core::pdhnames::{busiest, busiest_per_type, generalise, is_on};
use browse_core::stats::{median, summary};
use browse_core::win::{self, FMT_DOUBLE, FMT_LARGE, FMT_NOCAP100, Item, MIB, Query};
use browse_core::{path_arg, subcommand, write_json};
use serde_json::{Value, json};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, ID3DBlob, ID3DInclude};
use windows::Win32::Graphics::Direct3D11::*;
use windows::core::{PCSTR, s};

const DEDICATED: &str = r"\GPU Adapter Memory(*)\Dedicated Usage";
const SHARED: &str = r"\GPU Adapter Memory(*)\Shared Usage";
const ENGINE: &str = r"\GPU Engine(*)\Utilization Percentage";

/// `CStatus` values that carry a value (`PDH_CSTATUS_VALID_DATA`, `PDH_CSTATUS_NEW_DATA`).
fn valid(status: u32) -> bool {
    status == 0 || status == 1
}

/// The instances of `tag`'s adapter with a valid value.
fn on_adapter<'a>(items: &'a [Item], tag: &str) -> Vec<&'a Item> {
    items
        .iter()
        .filter(|i| is_on(&i.name, tag) && valid(i.status))
        .collect()
}

/// The generalised names and how many instances have each.
fn grammar<'a>(items: impl Iterator<Item = &'a Item>) -> BTreeMap<String, usize> {
    let mut g = BTreeMap::new();
    for i in items {
        *g.entry(generalise(&i.name)).or_insert(0) += 1;
    }
    g
}

fn read(out: &std::path::Path) {
    let elevated = win::token_elevated();
    let ad = win::discrete_adapter().expect("discrete adapter");
    let tag = ad.tag();
    let q = Query::open().expect("PdhOpenQueryW");
    let specs = [
        ("dedicated", DEDICATED, FMT_LARGE),
        ("shared", SHARED, FMT_LARGE),
        ("engine", ENGINE, FMT_DOUBLE | FMT_NOCAP100),
    ];
    let mut counters = Vec::new();
    let mut add = serde_json::Map::new();
    for (key, path, fmt) in specs {
        match q.add_english(path) {
            Ok(c) => {
                add.insert(key.into(), json!({"path": path, "status": 0}));
                counters.push((key, c, fmt));
            }
            Err(r) => {
                add.insert(
                    key.into(),
                    json!({"path": path, "status": format!("0x{r:08X}")}),
                );
            }
        }
    }
    let first = q.collect();
    thread::sleep(Duration::from_millis(100));

    let mut collect_ms = Vec::new();
    let mut read_ms = Vec::new();
    let mut collect_errors = 0usize;
    let mut arrived: BTreeMap<&str, usize> = BTreeMap::new();
    let mut last: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
    let mut max_instances: BTreeMap<&str, usize> = BTreeMap::new();
    let start = Instant::now();
    for i in 0..300u32 {
        let t0 = Instant::now();
        let r = q.collect();
        collect_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        if r != 0 {
            collect_errors += 1;
        }
        let t1 = Instant::now();
        for (key, c, fmt) in &counters {
            let items = q.items(*c, *fmt).unwrap_or_default();
            let m = max_instances.entry(key).or_insert(0);
            *m = (*m).max(items.len());
            if !on_adapter(&items, &tag).is_empty() {
                *arrived.entry(key).or_insert(0) += 1;
            }
            last.insert(key, items);
        }
        read_ms.push(t1.elapsed().as_secs_f64() * 1000.0);
        let next = Duration::from_millis(100 * u64::from(i + 1));
        if let Some(wait) = next.checked_sub(start.elapsed()) {
            thread::sleep(wait);
        }
    }

    let mut per_counter = serde_json::Map::new();
    let mut all_arrive = counters.len() == specs.len();
    for (key, _, _) in &counters {
        let items = &last[key];
        let mine = on_adapter(items, &tag);
        let others = items.iter().filter(|i| !is_on(&i.name, &tag)).count();
        let value = match *key {
            "engine" => {
                let per: BTreeMap<String, f64> = busiest_per_type(
                    &items
                        .iter()
                        .map(|i| (i.name.clone(), i.value))
                        .collect::<Vec<_>>(),
                    &tag,
                );
                json!({"busiest_per_type_percent": per, "busiest": busiest(&per)})
            }
            _ => json!({"mib": mine.iter().map(|i| i.value).sum::<f64>() / MIB}),
        };
        let iterations = arrived.get(key).copied().unwrap_or(0);
        all_arrive &= iterations == 300;
        per_counter.insert(
            (*key).into(),
            json!({
                "instances_last": items.len(),
                "instances_max": max_instances.get(key),
                "instances_on_discrete": mine.len(),
                "instances_on_other_adapters": others,
                "iterations_with_values": iterations,
                "grammar_discrete": grammar(items.iter().filter(|i| is_on(&i.name, &tag))),
                "grammar_all": grammar(items.iter()),
                "last": value,
            }),
        );
    }
    let collect = summary(&collect_ms);
    let p99 = collect["p99"].as_f64().unwrap_or(f64::INFINITY);
    let verdict = if elevated != Some(false) {
        "INVALID (token elevated or unreadable)"
    } else if all_arrive && p99 < 5.0 && collect_errors == 0 {
        "PASS"
    } else {
        "FAIL"
    };
    write_json(
        out,
        &json!({
            "elevated": elevated,
            "hardware_adapters": ad.hardware_adapters,
            "discrete_dedicated_mib": ad.dedicated_mib.round(),
            "add": add,
            "first_collect_status": first,
            "collect_errors": collect_errors,
            "interval_ms": 100,
            "collect_ms": collect,
            "read_arrays_ms": summary(&read_ms),
            "collect_ms_raw": collect_ms,
            "counters": per_counter,
            "all_three_arrive_every_collect": all_arrive,
            "verdict": verdict,
        }),
    );
    println!(
        "pdh-read: {verdict} (collect p99 {p99:.3} ms, all three arrive: {all_arrive}, elevated: {elevated:?})"
    );
}

/// The adapter's `Shared Usage` and `Dedicated Usage` in MiB, after a collect.
fn adapter_mib(
    q: &Query,
    shared: windows::Win32::System::Performance::PDH_HCOUNTER,
    dedicated: windows::Win32::System::Performance::PDH_HCOUNTER,
    tag: &str,
) -> (f64, f64) {
    q.collect();
    let sum = |c| {
        on_adapter(&q.items(c, FMT_LARGE).unwrap_or_default(), tag)
            .iter()
            .map(|i| i.value)
            .sum::<f64>()
            / MIB
    };
    (sum(shared), sum(dedicated))
}

fn device(ad: &win::Adapter) -> (ID3D11Device, ID3D11DeviceContext) {
    let mut dev = None;
    let mut ctx = None;
    unsafe {
        D3D11CreateDevice(
            &ad.adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            Some(&mut ctx),
        )
        .expect("D3D11CreateDevice");
    }
    (dev.unwrap(), ctx.unwrap())
}

const SHADER: &str = r"
RWStructuredBuffer<uint> outb : register(u0);
[numthreads(256, 1, 1)]
void main(uint3 id : SV_DispatchThreadID) {
    uint x = id.x * 2654435761u + 1u;
    [loop] for (uint i = 0; i < 16384; i++) { x = x * 1664525u + 1013904223u; x ^= x >> 13; }
    outb[id.x & 65535] = x;
}
";

/// The compute loop: dispatches until `stop`, at most two batches in flight. Counts batches.
fn compute_loop(stop: Arc<AtomicBool>, batches: Arc<AtomicU64>) {
    let ad = win::discrete_adapter().expect("adapter");
    let (dev, ctx) = device(&ad);
    unsafe {
        let mut blob: Option<ID3DBlob> = None;
        let mut errors: Option<ID3DBlob> = None;
        let r = D3DCompile(
            SHADER.as_ptr().cast(),
            SHADER.len(),
            PCSTR::null(),
            None,
            None::<&ID3DInclude>,
            s!("main"),
            s!("cs_5_0"),
            0,
            0,
            &mut blob,
            Some(&mut errors),
        );
        if let Err(e) = r {
            let msg = errors.map(|b| {
                String::from_utf8_lossy(std::slice::from_raw_parts(
                    b.GetBufferPointer().cast::<u8>(),
                    b.GetBufferSize(),
                ))
                .into_owned()
            });
            panic!("D3DCompile: {e} {msg:?}");
        }
        let blob = blob.unwrap();
        let code =
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        let mut cs = None;
        dev.CreateComputeShader(code, None::<&ID3D11ClassLinkage>, Some(&mut cs))
            .expect("CreateComputeShader");
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: 65536 * 4,
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_UNORDERED_ACCESS.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: D3D11_RESOURCE_MISC_BUFFER_STRUCTURED.0 as u32,
            StructureByteStride: 4,
        };
        let mut buf = None;
        dev.CreateBuffer(&desc, None, Some(&mut buf))
            .expect("CreateBuffer");
        let mut uav = None;
        dev.CreateUnorderedAccessView(buf.as_ref().unwrap(), None, Some(&mut uav))
            .expect("CreateUnorderedAccessView");
        ctx.CSSetShader(cs.as_ref(), None);
        ctx.CSSetUnorderedAccessViews(0, 1, Some(&uav), None);
        let qd = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut queries: Vec<ID3D11Query> = Vec::new();
        for _ in 0..2 {
            let mut q = None;
            dev.CreateQuery(&qd, Some(&mut q)).expect("CreateQuery");
            queries.push(q.unwrap());
        }
        let mut k = 0usize;
        while !stop.load(Ordering::Relaxed) {
            for _ in 0..8 {
                ctx.Dispatch(1024, 1, 1);
            }
            ctx.End(&queries[k % 2]);
            ctx.Flush();
            if k > 0 {
                let prev = &queries[(k + 1) % 2];
                loop {
                    let mut done: i32 = 0;
                    let _ = ctx.GetData(prev, Some((&raw mut done).cast()), 4, 0);
                    if done != 0 || stop.load(Ordering::Relaxed) {
                        break;
                    }
                    thread::yield_now();
                }
            }
            batches.fetch_add(1, Ordering::Relaxed);
            k += 1;
        }
        ctx.ClearState();
        ctx.Flush();
    }
}

fn load(out: &std::path::Path) {
    let elevated = win::token_elevated();
    let ad = win::discrete_adapter().expect("discrete adapter");
    let tag = ad.tag();
    let q = Query::open().expect("PdhOpenQueryW");
    let shared = q.add_english(SHARED).expect("Shared Usage");
    let dedicated = q.add_english(DEDICATED).expect("Dedicated Usage");
    let engine = q.add_english(ENGINE).expect("Utilization Percentage");
    q.collect();

    // Part 1: 1 GiB of STAGING buffers.
    let (dev, ctx) = device(&ad);
    let t0 = Instant::now();
    let mut series: Vec<Value> = Vec::new();
    let sample = |phase: &str, secs: u64, series: &mut Vec<Value>| {
        let start = Instant::now();
        let mut i = 0u64;
        while start.elapsed() < Duration::from_secs(secs) {
            let (s, d) = adapter_mib(&q, shared, dedicated, &tag);
            series.push(json!({"t_s": t0.elapsed().as_secs_f64(), "phase": phase, "shared_mib": s, "dedicated_mib": d}));
            i += 1;
            if let Some(w) = Duration::from_millis(250 * i).checked_sub(start.elapsed()) {
                thread::sleep(w);
            }
        }
    };
    sample("idle", 10, &mut series);
    let alloc_start = Instant::now();
    let mut buffers: Vec<ID3D11Buffer> = Vec::new();
    unsafe {
        let desc = D3D11_BUFFER_DESC {
            ByteWidth: 64 << 20,
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: (D3D11_CPU_ACCESS_READ.0 | D3D11_CPU_ACCESS_WRITE.0) as u32,
            MiscFlags: 0,
            StructureByteStride: 0,
        };
        for _ in 0..16 {
            let mut b = None;
            dev.CreateBuffer(&desc, None, Some(&mut b))
                .expect("CreateBuffer STAGING");
            let b = b.unwrap();
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&b, 0, D3D11_MAP_WRITE, 0, Some(&mut m))
                .expect("Map");
            std::ptr::write_bytes(m.pData.cast::<u8>(), 0xA5, 64 << 20);
            ctx.Unmap(&b, 0);
            buffers.push(b);
        }
    }
    let alloc_ms = alloc_start.elapsed().as_secs_f64() * 1000.0;
    sample("held", 10, &mut series);
    drop(buffers);
    unsafe {
        ctx.ClearState();
        ctx.Flush();
    }
    sample("released", 10, &mut series);
    drop((dev, ctx));
    let phase_median = |p: &str| {
        median(
            &series
                .iter()
                .filter(|v| v["phase"] == p)
                .filter_map(|v| v["shared_mib"].as_f64())
                .collect::<Vec<_>>(),
        )
    };
    let (idle, held, released) = (
        phase_median("idle"),
        phase_median("held"),
        phase_median("released"),
    );

    // Part 2: a 30 s compute loop; the busiest engine type against NVML at 1 Hz.
    let nvml = win::Nvml::open();
    let stop = Arc::new(AtomicBool::new(false));
    let batches = Arc::new(AtomicU64::new(0));
    let worker = {
        let (stop, batches) = (stop.clone(), batches.clone());
        thread::spawn(move || compute_loop(stop, batches))
    };
    thread::sleep(Duration::from_secs(2));
    q.collect();
    let mut compute: Vec<Value> = Vec::new();
    let (mut engine_series, mut nvml_series) = (Vec::new(), Vec::new());
    let start = Instant::now();
    for i in 1..=30u64 {
        if let Some(w) = Duration::from_secs(i).checked_sub(start.elapsed()) {
            thread::sleep(w);
        }
        q.collect();
        let items: Vec<(String, f64)> = q
            .items(engine, FMT_DOUBLE | FMT_NOCAP100)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| valid(i.status))
            .map(|i| (i.name, i.value))
            .collect();
        let per = busiest_per_type(&items, &tag);
        let top = busiest(&per);
        let nv = nvml.as_ref().ok().and_then(|n| n.gpu_util());
        if let Some((_, v)) = &top {
            engine_series.push(*v);
        }
        if let Some(v) = nv {
            nvml_series.push(f64::from(v));
        }
        compute.push(json!({"t_s": start.elapsed().as_secs_f64(), "per_type_percent": per, "busiest": top, "nvml_percent": nv, "batches": batches.load(Ordering::Relaxed)}));
    }
    stop.store(true, Ordering::Relaxed);
    worker.join().expect("compute thread");
    let mut types: BTreeMap<String, usize> = BTreeMap::new();
    for c in &compute {
        if let Some(t) = c["busiest"][0].as_str() {
            *types.entry(t.to_string()).or_insert(0) += 1;
        }
    }
    let result = json!({
        "elevated": elevated,
        "verdict": "RECORDED",
        "staging": {
            "buffers": 16, "buffer_mib": 64, "alloc_and_write_ms": alloc_ms,
            "shared_mib_median": {"idle": idle, "held": held, "released": released},
            "held_minus_idle_mib": held.zip(idle).map(|(h, i)| h - i),
            "released_minus_idle_mib": released.zip(idle).map(|(r, i)| r - i),
            "series": series,
        },
        "compute": {
            "seconds": 30,
            "busiest_type_counts": types,
            "busiest_engine_percent": summary(&engine_series),
            "nvml_percent": summary(&nvml_series),
            "median_busiest_engine_percent": median(&engine_series),
            "median_nvml_percent": median(&nvml_series),
            "nvml": match &nvml { Ok(n) => json!({"devices": n.devices, "device0_is_dxgi_adapter": n.pci_device_id.map(|p| p == ad.pci_id), "device0_bus_id": if n.devices > 1 { n.bus_id.clone() } else { None }}), Err(e) => json!({"error": e}) },
            "batches": batches.load(Ordering::Relaxed),
            "series": compute,
        },
    });
    write_json(out, &result);
    println!(
        "pdh-load: shared idle/held/released {idle:?}/{held:?}/{released:?} MiB; busiest engine median {:?} %, NVML median {:?} %",
        median(&engine_series),
        median(&nvml_series)
    );
}

fn main() {
    let out = path_arg("--out");
    match subcommand().as_str() {
        "read" => read(&out),
        "load" => load(&out),
        other => {
            eprintln!("usage: pdh-probe read|load --out <json> (got {other:?})");
            std::process::exit(2);
        }
    }
}
