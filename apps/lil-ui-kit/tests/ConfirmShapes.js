// What the real core sends about a confirmation (lil_core::confirm, lil_protocol::app), for the
// confirmation tests: `view` is ConfirmationRow::view, `event` the `confirmations` topic's data, and
// `listAnswer` app/confirmations.list's result. `covers` holds `confirm` and `cap` only (no files:
// the card reads those from `arguments`); `answered` and `confirmed_via` come with an answer;
// `project` and `cap` are left out when there is none.
.pragma library

function iso(ms) {
    return new Date(ms).toISOString().replace(/\.\d+Z$/, "Z")
}

function view(fields) {
    const now = Date.now()
    return Object.assign({id: "cf_0123456789abcdef", tool: "test.destructive", arguments: {},
                          caller_kind: "mcp", covers: ["confirm"], state: "pending",
                          created: iso(now), expires_at: iso(now + 30 * 60 * 1000)}, fields || {})
}

// cap_hits_json's shape: decimal strings.
function capHit(fields) {
    return Object.assign({cap: "monthly", limit: "1", settled: "0.9", held: "0", estimate: "5",
                          currency: "USD", rate: null}, fields || {})
}

function answeredView(v, state, via) {
    return Object.assign({}, v, {state: state, confirmed_via: via || "app", answered: iso(Date.now())})
}

function event(kind, v) {
    return {kind: kind, confirmation: v}
}

function listAnswer(rows) {
    return {status: "ok", confirmations: rows}
}

function answerResult(v) {
    return {status: "ok", confirmation: v}
}

// Answers every `app/confirmations.list` the fake has open, from `pending` and `history`.
function answerLists(fake, pending, history) {
    for (const c of fake.calls) {
        if (c.kind === "appCall" && c.name === "app/confirmations.list" && !c.answered) {
            c.answered = true
            fake.respond(c.id, listAnswer(c.args.state === "pending" ? pending : history))
        }
    }
}

// The fake's answers sent so far.
function answers(fake) {
    return fake.calls.filter(c => c.kind === "appCall" && c.name === "app/confirmation.answer")
}
