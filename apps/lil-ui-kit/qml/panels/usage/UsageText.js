// The Usage & spend panel's words and rows (APP-116 to APP-124): what a `usage.summary` row or a
// cap reads as, and the rows the panel shows and exports. Money is never computed here: every
// amount is the core's decimal string (CORE-013). On screen it is formatted by the `money`
// function the caller passes (MoneyText.text, Lilith.Core: "1,234.57 USD"); the CSV keeps the
// exact decimals.
.pragma library

// The dimension picker, in its order: the `group_by` field and its button's text.
const dimensions = [
    {id: "provider", text: "Provider"},
    {id: "model", text: "Model"},
    {id: "project", text: "Project"},
    {id: "workspace", text: "Workspace"},
    {id: "caller_kind", text: "Caller"},
    {id: "account", text: "Account"},
    {id: "tool", text: "Tool"},
]

// The billing bases, in the order their sections show (APP-118).
const bases = ["money", "subscription_quota", "local", "unattributed"]

// The units a record may carry, in Appendix F's order, with their words.
const units = [
    ["input_tokens", "input tokens"],
    ["cache_read_tokens", "cached input tokens"],
    ["cache_write_tokens", "cache-write tokens"],
    ["cache_ttl", "cache TTL"],
    ["output_tokens", "output tokens"],
    ["reasoning_tokens", "reasoning tokens"],
    ["images", "images"],
    ["seconds", "s"],
    ["characters", "characters"],
    ["requests", "requests"],
    ["gpu_seconds", "GPU-seconds"],
    ["energy_wh_gross", "Wh"],
    ["energy_wh_above_idle", "Wh above idle"],
]

// A caller kind as people say it (APP-116): you in an app, the assistant, Claude Code (MCP) and
// the command line.
function callerWord(kind) {
    if (typeof kind !== "string")
        return "Unknown caller"
    if (kind.startsWith("ui:"))
        return "You"
    return {assistant: "Assistant", mcp: "Claude Code", cli: "lil"}[kind] || kind
}

// A dimension's value as the row's label. Accounts are opaque ids (APP-124): the suite has no
// account labels yet, so the id is the label.
function label(dimension, value) {
    if (value === null || value === undefined || value === "") {
        const d = dimensions.find(d => d.id === dimension)
        return "No " + (d ? d.text.toLowerCase() : dimension)
    }
    if (dimension === "caller_kind")
        return callerWord(value)
    return String(value)
}

// `amount` in `currency` through `money(amount, currency)`, or as it came without one.
function moneyOf(amount, currency, money) {
    return money ? money(String(amount), String(currency)) : amount + " " + currency
}

// "3.00 EUR · 1.50 USD" for {EUR: "3", USD: "1.5"} (through `money`); "" for none.
function amounts(byCurrency, money) {
    return Object.keys(byCurrency || {}).sort().map(c => moneyOf(byCurrency[c], c, money)).join(" · ")
}

// "1200 input tokens · 4 images" for a row's units, in Appendix F's order, then any other.
// `cache_ttl` is a duration the core never adds up; shown as it comes if a row carries one.
function unitText(values) {
    const v = values || {}
    const known = units.filter(([k]) => v[k] !== undefined && v[k] !== null).map(([k, word]) => v[k] + " " + word)
    const rest = Object.keys(v).filter(k => !units.some(([u]) => u === k)).map(k => v[k] + " " + k)
    return known.concat(rest).join(" · ")
}

// The units of a row of `basis`: a money, quota or unattributed row's `units`, a local row's
// own fields.
function unitsOf(basis, row) {
    if (basis === "local") {
        const out = {}
        for (const [k] of units) {
            if (row[k] !== undefined)
                out[k] = row[k]
        }
        return out
    }
    return row.units || {}
}

// "2 of 3 estimated", "1 reconciled": a money row's marks (APP-117).
function marks(row) {
    const out = []
    if (row.estimated > 0)
        out.push(row.estimated === row.records ? "estimated" : row.estimated + " of " + row.records + " estimated")
    if (row.reconciled > 0)
        out.push(row.reconciled + " reconciled")
    return out
}

function records(n) {
    return n + (n === 1 ? " record" : " records")
}

// The rows of `summary` of `basis` as the panel shows them, grouped by `dimension`. Labels are
// raw here: the panel shows them through `shown`.
function shownRows(summary, basis, dimension) {
    if (!summary || !summary.rows)
        return []
    return summary.rows.filter(r => r.basis === basis).map(r => {
        const value = r[dimension]
        const money = basis === "money" || basis === "unattributed"
        return {
            period: r.period,
            basis: basis,
            value: value === undefined ? null : value,
            label: label(dimension, value),
            // The raw value beside a label that reads differently ("ui:image" under "You").
            detail: typeof value === "string" && label(dimension, value) !== value ? value : "",
            records: r.records,
            estimated: money ? (r.estimated || 0) : null,
            reconciled: money ? (r.reconciled || 0) : null,
            byCurrency: money ? (r.by_currency || {}) : {},
            energyCost: basis === "local" ? (r.energy_cost || {}) : {},
            units: unitsOf(basis, r),
            marks: money ? marks(r) : [],
        }
    })
}

// "YYYY-MM" of today, on this PC's calendar, as the core's months are (CORE-012).
function localMonth() {
    const d = new Date()
    return d.getFullYear() + "-" + String(d.getMonth() + 1).padStart(2, "0")
}

// The CSV of the rows shown (APP-117): its columns and one object per line. `amount` is money
// spend only (money and unattributed rows, told apart by `basis`), one line per currency; local
// work's energy cost has its own `energy_currency` and `energy_cost` columns. A row in several
// currencies has its counts and units on its first line only, so a spreadsheet's sums count
// them once. A month's fixed costs follow as `basis: "fixed_cost"` lines with the month's
// period and their `display_amount`. The unit columns are the ones the rows carry, in
// Appendix F's order. Values are raw (exact decimals): the CSV guards them.
function exportTable(summary, dimension) {
    const rows = []
    for (const basis of bases)
        rows.push(...shownRows(summary, basis, dimension))
    const seen = units.map(([k]) => k).filter(k => rows.some(r => r.units[k] !== undefined))
    const energy = rows.some(r => Object.keys(r.energyCost).length > 0)
    const fixed = (summary && summary.fixed_costs) || []
    const columns = ["period", "basis", dimension, "label", "records", "estimated", "reconciled",
                     "currency", "amount"].concat(seen)
                     .concat(energy ? ["energy_currency", "energy_cost"] : [])
                     .concat(fixed.length > 0 ? ["display_amount"] : [])
    const lines = []
    for (const r of rows) {
        const base = {period: r.period, basis: r.basis, label: r.label}
        base[dimension] = r.value
        const counts = {records: r.records, estimated: r.estimated, reconciled: r.reconciled}
        for (const k of seen)
            counts[k] = r.units[k] === undefined ? null : r.units[k]
        const parts = []
        for (const c of Object.keys(r.byCurrency).sort())
            parts.push({currency: c, amount: r.byCurrency[c]})
        for (const c of Object.keys(r.energyCost).sort())
            parts.push({energy_currency: c, energy_cost: r.energyCost[c]})
        if (parts.length === 0)
            parts.push({})
        parts.forEach((p, i) => lines.push(Object.assign({}, base, i === 0 ? counts : {}, p)))
    }
    const month = summary && summary.rows && summary.rows.length > 0 ? summary.rows[0].period : localMonth()
    for (const f of fixed)
        lines.push({period: month, basis: "fixed_cost", label: f.name, currency: f.currency, amount: f.amount,
                    display_amount: f.display_amount})
    return {columns: columns, rows: lines}
}

// `s` as shown on screen: `core.shownText` (controls removed, bidi and zero-width marks made
// visible, at most `n` characters, 80 by default; P4a-R8).
function shown(core, s, n) {
    const text = s === null || s === undefined ? "" : String(s)
    return core ? core.shownText(text, n || 80) : text
}
// A cap's name: "Monthly cap", "Per-call maximum", "openrouter cap".
function capName(cap) {
    if (cap === "monthly")
        return "Monthly cap"
    if (cap === "per_call_max")
        return "Per-call maximum"
    if (typeof cap === "string" && cap.startsWith("provider:"))
        return cap.slice("provider:".length) + " cap"
    return String(cap)
}

// An `alert` event of the `caps` topic as one line (CORE-089), its amount through `money`.
function alertText(a, money) {
    let text = capName(a.cap) + " passed " + a.threshold_pct + " %"
    if (a.spent !== undefined && a.spent !== null)
        text += ": " + moneyOf(a.spent, a.currency || "", money) + " settled"
                + (a.spent_pct !== undefined && a.spent_pct !== null ? " (" + a.spent_pct + " %)" : "")
    if (a.period)
        text += ", " + a.period
    return text
}
