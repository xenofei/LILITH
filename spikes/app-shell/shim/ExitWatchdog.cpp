#include "ExitWatchdog.h"

#include <QCoreApplication>
#include <QElapsedTimer>
#include <QTimer>

#include <algorithm>
#include <algorithm>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

#ifdef Q_OS_WIN
#  ifndef NOMINMAX
#    define NOMINMAX
#  endif
#  include <windows.h>
#  include <dbghelp.h>
#  include <tlhelp32.h>
#endif

#ifdef Q_OS_WIN
namespace {
std::atomic<bool> g_armed{false};
std::atomic<qint64> g_lastGuiTick{0};
std::atomic<DWORD> g_guiTid{0};
QElapsedTimer g_clock;

struct Module { DWORD64 base; DWORD64 size; char name[MAX_PATH]; };

// Modules from a Toolhelp snapshot: no loader lock and no allocation inside the target threads.
std::vector<Module> modules()
{
    std::vector<Module> out;
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    MODULEENTRY32 me = {};
    me.dwSize = sizeof(me);
    if (snap != INVALID_HANDLE_VALUE && Module32First(snap, &me)) {
        do {
            Module m{DWORD64(me.modBaseAddr), DWORD64(me.modBaseSize), {}};
            WideCharToMultiByte(CP_UTF8, 0, me.szModule, -1, m.name, MAX_PATH, nullptr, nullptr);
            out.push_back(m);
        } while (Module32Next(snap, &me));
    }
    if (snap != INVALID_HANDLE_VALUE)
        CloseHandle(snap);
    return out;
}

const Module *moduleOf(const std::vector<Module> &mods, DWORD64 a)
{
    for (const Module &m : mods)
        if (a >= m.base && a < m.base + m.size)
            return &m;
    return nullptr;
}

std::vector<DWORD> threadIds()
{
    std::vector<DWORD> out;
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    THREADENTRY32 te = {};
    te.dwSize = sizeof(te);
    const DWORD pid = GetCurrentProcessId(), self = GetCurrentThreadId(), gui = g_guiTid.load();
    if (gui)
        out.push_back(gui);
    if (Thread32First(snap, &te)) {
        do {
            if (te.th32OwnerProcessID == pid && te.th32ThreadID != self && te.th32ThreadID != gui)
                out.push_back(te.th32ThreadID);
        } while (Thread32Next(snap, &te));
    }
    CloseHandle(snap);
    return out;
}

// A thread's registers and the top of its stack, copied while it is suspended. The thread is
// suspended only for GetThreadContext and ReadProcessMemory (kernel calls that take no user-mode
// lock), and is resumed on every path before anything else runs, so a dump can never leave a
// thread suspended. Everything after that (formatting, dbghelp) works on the copy.
struct Snapshot {
    DWORD tid = 0;
    bool ok = false;
    CONTEXT ctx = {};
    DWORD64 stackBase = 0;           // = Rsp at the time of the copy
    std::vector<unsigned char> stack; // up to kStackCopy bytes from Rsp
};
constexpr SIZE_T kStackCopy = 64 * 1024;

Snapshot snapshot(DWORD tid)
{
    Snapshot s;
    s.tid = tid;
    s.stack.resize(kStackCopy); // allocate before suspending
    HANDLE th = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, FALSE, tid);
    if (!th)
        return s;
    SIZE_T copied = 0;
    if (SuspendThread(th) != DWORD(-1)) {
        s.ctx.ContextFlags = CONTEXT_FULL;
        if (GetThreadContext(th, &s.ctx)) {
            s.ok = true;
            s.stackBase = s.ctx.Rsp;
            // Page by page, so the copy stops cleanly at the end of the committed stack.
            const SIZE_T page = 4096;
            const DWORD64 first = s.stackBase & ~DWORD64(page - 1);
            SIZE_T head = SIZE_T(s.stackBase - first);
            for (DWORD64 a = first; copied < kStackCopy; a += page) {
                const SIZE_T skip = (a == first) ? head : 0;
                const SIZE_T want = std::min<SIZE_T>(page - skip, kStackCopy - copied);
                SIZE_T got = 0;
                if (!ReadProcessMemory(GetCurrentProcess(), reinterpret_cast<LPCVOID>(a + skip), s.stack.data() + copied, want, &got) || got == 0)
                    break;
                copied += got;
            }
        }
        ResumeThread(th);
    }
    CloseHandle(th);
    s.stack.resize(copied);
    return s;
}

// Pass 1: RIP plus a scan of the copied stack for addresses inside loaded modules (likely return
// addresses). No dbghelp.
void quickDump(FILE *f, const std::vector<Module> &mods, const Snapshot &s, bool gui)
{
    const DWORD64 rip = s.ok ? s.ctx.Rip : 0;
    const Module *m = moduleOf(mods, rip);
    std::fprintf(f, "--- thread %lu%s rip %s+0x%llx\n", s.tid, gui ? " (GUI)" : "", m ? m->name : "?",
                 (unsigned long long)(m ? rip - m->base : rip));
    int shown = 0;
    const auto *words = reinterpret_cast<const DWORD64 *>(s.stack.data());
    const SIZE_T n = std::min<SIZE_T>(s.stack.size() / sizeof(DWORD64), 256);
    for (SIZE_T i = 0; i < n && shown < (gui ? 40 : 12); ++i) {
        if (const Module *r = moduleOf(mods, words[i])) {
            std::fprintf(f, "    ? %s+0x%llx\n", r->name, (unsigned long long)(words[i] - r->base));
            ++shown;
        }
    }
    std::fflush(f);
}

// StackWalk64 reads the stack from the copy (the live stack has moved on), anything else (code,
// unwind data) from the process.
thread_local const Snapshot *t_walking = nullptr;
BOOL CALLBACK readFromSnapshot(HANDLE process, DWORD64 addr, PVOID buf, DWORD size, LPDWORD read)
{
    const Snapshot *s = t_walking;
    if (s && addr >= s->stackBase && addr + size <= s->stackBase + s->stack.size()) {
        std::memcpy(buf, s->stack.data() + (addr - s->stackBase), size);
        if (read)
            *read = size;
        return TRUE;
    }
    SIZE_T got = 0;
    const BOOL ok = ReadProcessMemory(process, reinterpret_cast<LPCVOID>(addr), buf, size, &got);
    if (read)
        *read = DWORD(got);
    return ok;
}

// Pass 2: symbolised stacks (dbghelp), walked over the copy; no thread is suspended here, so if
// dbghelp blocks on a lock a stalled thread holds, only this watchdog thread waits.
void fullDump(FILE *f, HANDLE process, const Snapshot &s)
{
    if (!s.ok)
        return;
    std::fprintf(f, "=== thread %lu\n", s.tid);
    CONTEXT ctx = s.ctx;
    STACKFRAME64 frame = {};
    frame.AddrPC.Offset = ctx.Rip;
    frame.AddrPC.Mode = AddrModeFlat;
    frame.AddrFrame.Offset = ctx.Rbp;
    frame.AddrFrame.Mode = AddrModeFlat;
    frame.AddrStack.Offset = ctx.Rsp;
    frame.AddrStack.Mode = AddrModeFlat;
    alignas(SYMBOL_INFO) char buf[sizeof(SYMBOL_INFO) + 512];
    t_walking = &s;
    for (int i = 0; i < 48; ++i) {
        if (!StackWalk64(IMAGE_FILE_MACHINE_AMD64, process, nullptr, &frame, &ctx, readFromSnapshot,
                         SymFunctionTableAccess64, SymGetModuleBase64, nullptr))
            break;
        const DWORD64 pc = frame.AddrPC.Offset;
        if (!pc)
            break;
        auto *sym = reinterpret_cast<SYMBOL_INFO *>(buf);
        sym->SizeOfStruct = sizeof(SYMBOL_INFO);
        sym->MaxNameLen = 511;
        DWORD64 disp = 0;
        char module[MAX_PATH] = "?";
        const DWORD64 base = SymGetModuleBase64(process, pc);
        if (base)
            GetModuleFileNameA(reinterpret_cast<HMODULE>(base), module, MAX_PATH);
        const char *mn = std::strrchr(module, '\\');
        if (SymFromAddr(process, pc, &disp, sym))
            std::fprintf(f, "  %s!%s+0x%llx\n", mn ? mn + 1 : module, sym->Name, (unsigned long long)disp);
        else
            std::fprintf(f, "  %s+0x%llx\n", mn ? mn + 1 : module, (unsigned long long)(pc - base));
    }
    t_walking = nullptr;
    std::fflush(f);
}

void dumpAll(const std::wstring &wpath, const char *why, std::vector<Module> mods)
{
    FILE *f = nullptr;
    if (_wfopen_s(&f, wpath.c_str(), L"w") != 0 || !f)
        return;
    const std::vector<DWORD> tids = threadIds();
    std::fprintf(f, "%s; %zu other threads. Pass 1 (RIP and a stack scan), GUI thread first:\n", why, tids.size());
    std::fflush(f);
    // One snapshot per thread, each suspended only while it is copied.
    std::vector<Snapshot> snaps;
    snaps.reserve(tids.size());
    for (DWORD tid : tids)
        snaps.push_back(snapshot(tid));
    for (const Snapshot &s : snaps)
        quickDump(f, mods, s, s.tid == g_guiTid.load());
    std::fprintf(f, "Pass 2 (dbghelp, over the copies):\n");
    std::fflush(f);
    HANDLE process = GetCurrentProcess();
    SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS);
    SymInitialize(process, nullptr, TRUE);
    for (const Snapshot &s : snaps)
        fullDump(f, process, s);
    std::fprintf(f, "end\n");
    std::fclose(f);
    SymCleanup(process);
}
} // namespace
#endif

void ExitWatchdog::arm(const QString &path, int seconds)
{
#ifdef Q_OS_WIN
    if (g_armed.exchange(true))
        return;
    const std::wstring wpath = path.toStdWString();
    std::thread([wpath, seconds] {
        Sleep(DWORD(seconds) * 1000);
        char why[96];
        std::snprintf(why, sizeof why, "process still alive %d s after quit", seconds);
        dumpAll(wpath, why, modules());
    }).detach();
#else
    Q_UNUSED(path);
    Q_UNUSED(seconds);
#endif
}

void ExitWatchdog::watchGuiThread(const QString &path, int staleMs)
{
#ifdef Q_OS_WIN
    g_guiTid = GetCurrentThreadId();
    g_clock.start();
    g_lastGuiTick = 0;
    auto *t = new QTimer(QCoreApplication::instance());
    QObject::connect(t, &QTimer::timeout, [] { g_lastGuiTick = g_clock.elapsed(); });
    t->start(100);
    const std::wstring wpath = path.toStdWString();
    std::thread([wpath, staleMs] {
        // The module list is refreshed while the GUI thread is healthy, so a stall that holds the
        // loader lock doesn't block the dump.
        std::vector<Module> mods = modules();
        int n = 0;
        for (;;) {
            Sleep(250);
            const qint64 stale = g_clock.elapsed() - g_lastGuiTick.load();
            if (stale < 1000 && ++n % 4 == 0)
                mods = modules();
            if (stale > staleMs) {
                char why[96];
                std::snprintf(why, sizeof why, "GUI thread stalled for %lld ms", (long long)stale);
                dumpAll(wpath, why, mods);
                return;
            }
        }
    }).detach();
#else
    Q_UNUSED(path);
    Q_UNUSED(staleMs);
#endif
}
