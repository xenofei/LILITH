// The Usage & spend panel's words and rows (APP-116 to APP-124): what a `usage.summary` row or a
// cap reads as, and the rows the panel shows and exports. Money is never computed here: every
// amount is the core's decimal string, shown with its currency code as it comes (CORE-013).
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

// "3 EUR · 1.5 USD" for {EUR: "3", USD: "1.5"}; "" for none.
function amounts(byCurrency) {
    return Object.keys(byCurrency || {}).sort().map(c => byCurrency[c] + " " + c).join(" · ")
}

// "1200 input tokens · 4 images" for a row's units, in Appendix F's order, then any other.
function unitText(values) {
    const v = values || {}
    const known = units.filter(([k]) => v[k] !== undefined && v[k] !== null).map(([k, word]) => v[k] + " " + word)
    const rest = Object.keys(v).filter(k => !units.some(([u]) => u === k)).map(k => v[k] + " " + k)
    return known.concat(rest).join(" · ")
}

// The units of a row of `basis`: a quota row's `units`, a local row's own fields.
function unitsOf(basis, row) {
    if (basis === "subscription_quota")
        return row.units || {}
    if (basis === "local") {
        const out = {}
        for (const [k] of units) {
            if (row[k] !== undefined)
                out[k] = row[k]
        }
        return out
    }
    return {}
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

// The rows of `summary` of `basis` as the panel shows them, grouped by `dimension`.
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
            byCurrency: money ? (r.by_currency || {}) : (basis === "local" ? (r.energy_cost || {}) : {}),
            units: unitsOf(basis, r),
            marks: money ? marks(r) : [],
        }
    })
}

// The CSV of the rows shown (APP-117): its columns and one object per line. A row with amounts
// in several currencies is one line per currency; for local work the amount is its energy cost.
// The unit columns are the ones the rows carry, in Appendix F's order.
function exportTable(summary, dimension) {
    const rows = []
    for (const basis of bases)
        rows.push(...shownRows(summary, basis, dimension))
    const seen = units.map(([k]) => k).filter(k => rows.some(r => r.units[k] !== undefined))
    const columns = ["period", "basis", dimension, "label", "records", "estimated", "reconciled",
                     "currency", "amount"].concat(seen)
    const lines = []
    for (const r of rows) {
        const base = {period: r.period, basis: r.basis, label: r.label, records: r.records,
                      estimated: r.estimated, reconciled: r.reconciled}
        base[dimension] = r.value
        for (const k of seen)
            base[k] = r.units[k] === undefined ? null : r.units[k]
        const currencies = Object.keys(r.byCurrency).sort()
        if (currencies.length === 0)
            lines.push(Object.assign({currency: null, amount: null}, base))
        for (const c of currencies)
            lines.push(Object.assign({currency: c, amount: r.byCurrency[c]}, base))
    }
    return {columns: columns, rows: lines}
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

// An `alert` event of the `caps` topic as one line (CORE-089).
function alertText(a) {
    let text = capName(a.cap) + " passed " + a.threshold_pct + " %"
    if (a.spent !== undefined && a.spent !== null)
        text += ": " + a.spent + " " + (a.currency || "") + " settled"
                + (a.spent_pct !== undefined && a.spent_pct !== null ? " (" + a.spent_pct + " %)" : "")
    if (a.period)
        text += ", " + a.period
    return text
}
