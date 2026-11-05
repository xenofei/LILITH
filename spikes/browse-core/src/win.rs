//! Thin Windows wrappers: the token's elevation, the working set, PDH queries, the discrete
//! adapter's LUID, a process tree and NVML's utilisation. Nothing here runs at load time.

use std::collections::{BTreeSet, HashMap};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIAdapter1, IDXGIFactory1,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Performance::{
    PDH_FMT, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PCSTR, PCWSTR};

/// Whether this process's token is elevated, or `None` when it can't be read.
pub fn token_elevated() -> Option<bool> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let r = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        r.ok()?;
        Some(elevation.TokenIsElevated != 0)
    }
}

/// `(working set, peak working set)` of this process, in bytes.
pub fn working_set() -> (u64, u64) {
    let mut c = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb);
    }
    (c.WorkingSetSize as u64, c.PeakWorkingSetSize as u64)
}

pub const MIB: f64 = 1024.0 * 1024.0;

/// The discrete GPU: the hardware adapter with the most dedicated video memory.
pub struct Adapter {
    pub adapter: IDXGIAdapter1,
    pub luid_high: i32,
    pub luid_low: u32,
    pub dedicated_mib: f64,
    pub hardware_adapters: usize,
}

impl Adapter {
    /// The PDH form of its LUID.
    pub fn tag(&self) -> String {
        crate::pdhnames::luid_tag(self.luid_high, self.luid_low)
    }
}

pub fn discrete_adapter() -> Result<Adapter, String> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {e}"))?;
        let mut best: Option<Adapter> = None;
        let mut hardware = 0usize;
        let mut i = 0u32;
        while let Ok(a) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(d) = a.GetDesc1() else { continue };
            if d.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                continue;
            }
            hardware += 1;
            let mib = d.DedicatedVideoMemory as f64 / MIB;
            if best.as_ref().is_none_or(|b| mib > b.dedicated_mib) {
                best = Some(Adapter {
                    adapter: a,
                    luid_high: d.AdapterLuid.HighPart,
                    luid_low: d.AdapterLuid.LowPart,
                    dedicated_mib: mib,
                    hardware_adapters: 0,
                });
            }
        }
        let mut b = best.ok_or("no hardware adapter")?;
        b.hardware_adapters = hardware;
        Ok(b)
    }
}

/// One PDH query (closed on drop).
pub struct Query {
    h: PDH_HQUERY,
}

/// One formatted instance: its name, `CStatus` and value.
pub struct Item {
    pub name: String,
    pub status: u32,
    pub value: f64,
}

/// `PDH_FMT_NOCAP100`: engine percentages are not capped at 100.
pub const FMT_NOCAP100: u32 = 0x0000_8000;
pub const FMT_DOUBLE: u32 = 0x0000_0200;
pub const FMT_LARGE: u32 = 0x0000_0400;

impl Query {
    pub fn open() -> Result<Query, String> {
        let mut h = PDH_HQUERY::default();
        let r = unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut h) };
        if r != 0 {
            return Err(format!("PdhOpenQueryW: 0x{r:08X}"));
        }
        Ok(Query { h })
    }

    /// `PdhAddEnglishCounterW`, or the PDH status as an error.
    pub fn add_english(&self, path: &str) -> Result<PDH_HCOUNTER, u32> {
        let mut c = PDH_HCOUNTER::default();
        let r = unsafe { PdhAddEnglishCounterW(self.h, &HSTRING::from(path), 0, &mut c) };
        if r != 0 { Err(r) } else { Ok(c) }
    }

    /// `PdhCollectQueryData`'s status (0 is success).
    pub fn collect(&self) -> u32 {
        unsafe { PdhCollectQueryData(self.h) }
    }

    /// Every instance of counter `c`, formatted with `fmt` (`FMT_LARGE` reads `largeValue`,
    /// otherwise `doubleValue`).
    pub fn items(&self, c: PDH_HCOUNTER, fmt: u32) -> Result<Vec<Item>, u32> {
        unsafe {
            let mut size = 0u32;
            let mut count = 0u32;
            let r = PdhGetFormattedCounterArrayW(c, PDH_FMT(fmt), &mut size, &mut count, None);
            if r != PDH_MORE_DATA {
                return if r == 0 { Ok(Vec::new()) } else { Err(r) };
            }
            // u64 storage keeps the items 8-byte aligned.
            let mut buf = vec![0u64; (size as usize).div_ceil(8) + 1];
            let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            let r =
                PdhGetFormattedCounterArrayW(c, PDH_FMT(fmt), &mut size, &mut count, Some(items));
            if r != 0 {
                return Err(r);
            }
            let slice = std::slice::from_raw_parts(items, count as usize);
            Ok(slice
                .iter()
                .map(|it| Item {
                    name: it.szName.to_string().unwrap_or_default(),
                    status: it.FmtValue.CStatus,
                    value: if fmt & FMT_LARGE != 0 {
                        it.FmtValue.Anonymous.largeValue as f64
                    } else {
                        it.FmtValue.Anonymous.doubleValue
                    },
                })
                .collect())
        }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.h);
        }
    }
}

/// `root` and every live descendant of it (by parent PID, from one Toolhelp snapshot).
pub fn process_tree(root: u32) -> BTreeSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return BTreeSet::from([root]);
        };
        let mut e = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                if e.th32ProcessID != e.th32ParentProcessID {
                    children
                        .entry(e.th32ParentProcessID)
                        .or_default()
                        .push(e.th32ProcessID);
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    let mut tree = BTreeSet::from([root]);
    let mut todo = vec![root];
    while let Some(p) = todo.pop() {
        for c in children.get(&p).into_iter().flatten() {
            if tree.insert(*c) {
                todo.push(*c);
            }
        }
    }
    tree
}

#[repr(C)]
struct NvmlUtilization {
    gpu: u32,
    memory: u32,
}

type NvmlInit = unsafe extern "system" fn() -> i32;
type NvmlCount = unsafe extern "system" fn(*mut u32) -> i32;
type NvmlHandle = unsafe extern "system" fn(u32, *mut *mut core::ffi::c_void) -> i32;
type NvmlUtil = unsafe extern "system" fn(*mut core::ffi::c_void, *mut NvmlUtilization) -> i32;
type NvmlShutdown = unsafe extern "system" fn() -> i32;

/// NVML loaded in process (`nvml.dll` from the driver), device 0.
pub struct Nvml {
    _lib: HMODULE,
    util: NvmlUtil,
    shutdown: NvmlShutdown,
    device: *mut core::ffi::c_void,
    pub devices: u32,
}

impl Nvml {
    pub fn open() -> Result<Nvml, String> {
        unsafe {
            let lib =
                LoadLibraryW(&HSTRING::from("nvml.dll")).map_err(|e| format!("nvml.dll: {e}"))?;
            let get = |name: &[u8]| {
                GetProcAddress(lib, PCSTR(name.as_ptr())).ok_or("missing NVML export")
            };
            let init: NvmlInit = std::mem::transmute(get(b"nvmlInit_v2\0")?);
            let count: NvmlCount = std::mem::transmute(get(b"nvmlDeviceGetCount_v2\0")?);
            let handle: NvmlHandle = std::mem::transmute(get(b"nvmlDeviceGetHandleByIndex_v2\0")?);
            let util: NvmlUtil = std::mem::transmute(get(b"nvmlDeviceGetUtilizationRates\0")?);
            let shutdown: NvmlShutdown = std::mem::transmute(get(b"nvmlShutdown\0")?);
            let r = init();
            if r != 0 {
                return Err(format!("nvmlInit_v2: {r}"));
            }
            let mut devices = 0u32;
            count(&mut devices);
            let mut device = std::ptr::null_mut();
            let r = handle(0, &mut device);
            if r != 0 {
                shutdown();
                return Err(format!("nvmlDeviceGetHandleByIndex_v2: {r}"));
            }
            Ok(Nvml {
                _lib: lib,
                util,
                shutdown,
                device,
                devices,
            })
        }
    }

    /// The GPU utilisation in percent, as NVML reports it.
    pub fn gpu_util(&self) -> Option<u32> {
        let mut u = NvmlUtilization { gpu: 0, memory: 0 };
        let r = unsafe { (self.util)(self.device, &mut u) };
        (r == 0).then_some(u.gpu)
    }
}

impl Drop for Nvml {
    fn drop(&mut self) {
        unsafe {
            (self.shutdown)();
        }
    }
}
