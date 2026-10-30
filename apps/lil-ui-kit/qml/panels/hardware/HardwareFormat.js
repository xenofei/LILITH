// The Hardware panel's figures as text (APP-127, APP-131), the same whatever the system locale:
// thousands with a comma, a point for decimals, a space before the unit.
.pragma library

// A whole number with thousands separators: 9728 is "9,728".
function group(n) {
    const s = String(Math.round(Math.abs(n)))
    return (n < 0 ? "-" : "") + s.replace(/\B(?=(\d{3})+(?!\d))/g, ",")
}

// `x` with `digits` decimals and its whole part grouped: 2974.66 is "2,974.66".
function fixed(x, digits) {
    const text = Math.abs(x).toFixed(digits)
    const dot = text.indexOf(".")
    const whole = dot < 0 ? text : text.slice(0, dot)
    return (x < 0 && Number(text) !== 0 ? "-" : "") + group(Number(whole)) + (dot < 0 ? "" : text.slice(dot))
}

// One decimal, dropped when it is 0: 9.5, 16, 1,234.5.
function oneDecimal(x) {
    const r = Math.round(x * 10) / 10
    return Number.isInteger(r) ? group(r) : fixed(r, 1)
}

function percent(p) {
    return Math.round(p) + " %"
}

// MiB as GiB: 9728 is "9.5 GiB".
function gib(mib) {
    return oneDecimal(mib / 1024) + " GiB"
}

// "9.5 / 16 GiB".
function gibOf(usedMib, totalMib) {
    return oneDecimal(usedMib / 1024) + " / " + oneDecimal(totalMib / 1024) + " GiB"
}

function mib(n) {
    return group(n) + " MiB"
}

function watts(mw) {
    return group(mw / 1000) + " W"
}

function mhz(n) {
    return group(n) + " MHz"
}

// Bytes per second, decimal units, the unit chosen after rounding: "1.2 MB/s", "40 kB/s", and
// 999,960 B/s is "1 MB/s", not "1,000 kB/s".
function rate(bps) {
    const units = [[1e9, " GB/s"], [1e6, " MB/s"], [1e3, " kB/s"]]
    for (const [size, unit] of units) {
        const shown = Math.round(bps / size * 10) / 10
        if (shown >= 1)
            return oneDecimal(shown) + unit
    }
    return group(bps) + " B/s"
}

// mJ as energy, the unit chosen after rounding: "2.0 Wh", "500 Wh", "15.00 kWh", "2,974.72 kWh";
// 999.96 Wh is "1.00 kWh".
function energy(mj) {
    const wh = mj / 3.6e6
    if (Math.round(wh) >= 1000)
        return fixed(wh / 1000, 2) + " kWh"
    if (Math.round(wh * 10) / 10 >= 10)
        return group(wh) + " Wh"
    return wh.toFixed(1) + " Wh"
}

// NVML's clock event reasons (telemetry.snapshot `gpu.throttle`) in words.
const throttleNames = {
    gpu_idle: "Idle",
    applications_clocks_setting: "Application clocks",
    sw_power_cap: "Power cap",
    hw_slowdown: "Hardware slowdown",
    sync_boost: "Sync boost",
    sw_thermal_slowdown: "Thermal (software)",
    hw_thermal_slowdown: "Thermal (hardware)",
    hw_power_brake_slowdown: "Power brake",
    display_clock_setting: "Display clock"
}

function throttle(reasons) {
    if (!reasons || reasons.length === 0)
        return "None"
    return reasons.map(r => throttleNames[r] || r).join(", ")
}

// A UTC time as the local hour and minute: "14:03".
function localTime(utc) {
    const ms = Date.parse(utc)
    return isNaN(ms) ? "" : Qt.formatTime(new Date(ms), "HH:mm")
}
