// The Hardware panel's figures as text (APP-127, APP-131), the same whatever the system locale:
// thousands with a comma, a point for decimals, a space before the unit.
.pragma library

// A whole number with thousands separators: 9728 is "9,728".
function group(n) {
    const s = String(Math.round(Math.abs(n)))
    return (n < 0 ? "-" : "") + s.replace(/\B(?=(\d{3})+(?!\d))/g, ",")
}

// One decimal, dropped when it is 0: 9.5, 16.
function oneDecimal(x) {
    const r = Math.round(x * 10) / 10
    return Number.isInteger(r) ? group(r) : r.toFixed(1)
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

// Bytes per second, decimal units: "1.2 MB/s", "40 kB/s".
function rate(bps) {
    if (bps >= 1e9)
        return oneDecimal(bps / 1e9) + " GB/s"
    if (bps >= 1e6)
        return oneDecimal(bps / 1e6) + " MB/s"
    if (bps >= 1e3)
        return oneDecimal(bps / 1e3) + " kB/s"
    return group(bps) + " B/s"
}

// mJ as energy: "2.0 Wh", "500 Wh", "15.00 kWh".
function energy(mj) {
    const wh = mj / 3.6e6
    if (wh >= 1000)
        return (wh / 1000).toFixed(2) + " kWh"
    if (wh >= 10)
        return group(wh) + " Wh"
    return wh.toFixed(1) + " Wh"
}

// telemetry.energy's energy_cost: "0.42 EUR".
function cost(c) {
    return c ? c.amount + " " + c.currency : ""
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
