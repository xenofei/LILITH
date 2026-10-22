#include "ExitWatchdog.h"

#include <QCoreApplication>
#include <QElapsedTimer>
#include <QTimer>

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

// Pass 1, safe while another thread holds the loader or heap lock: RIP plus a scan of the top
// of the stack for addresses inside loaded modules (likely return addresses). No dbghelp, no
// allocation while the thread is suspended.
void quickDump(FILE *f, const std::vector<Module> &mods, DWORD tid, bool gui)
{
    HANDLE th = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, FALSE, tid);
    if (!th)
        return;
    DWORD64 rip = 0, rsp = 0;
    DWORD64 words[256] = {};
    SIZE_T got = 0;
    if (SuspendThread(th) != DWORD(-1)) {
        CONTEXT ctx = {};
        ctx.ContextFlags = CONTEXT_CONTROL;
        if (GetThreadContext(th, &ctx)) {
            rip = ctx.Rip;
            rsp = ctx.Rsp;
            ReadProcessMemory(GetCurrentProcess(), reinterpret_cast<LPCVOID>(rsp), words, sizeof(words), &got);
        }
        ResumeThread(th);
    }
    CloseHandle(th);
    const Module *m = moduleOf(mods, rip);
    std::fprintf(f, "--- thread %lu%s rip %s+0x%llx\n", tid, gui ? " (GUI)" : "", m ? m->name : "?",
                 (unsigned long long)(m ? rip - m->base : rip));
    int shown = 0;
    for (SIZE_T i = 0; i < got / sizeof(DWORD64) && shown < (gui ? 40 : 12); ++i) {
        if (const Module *r = moduleOf(mods, words[i])) {
            std::fprintf(f, "    ? %s+0x%llx\n", r->name, (unsigned long long)(words[i] - r->base));
            ++shown;
        }
    }
    std::fflush(f);
}

// Pass 2: symbolised stacks (dbghelp). Can block if a suspended thread holds a lock dbghelp
// needs; pass 1 is already on disk by then.
void fullDump(FILE *f, HANDLE process, DWORD tid)
{
    HANDLE th = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION, FALSE, tid);
    if (!th)
        return;
    if (SuspendThread(th) == DWORD(-1)) {
        CloseHandle(th);
        return;
    }
    CONTEXT ctx = {};
    ctx.ContextFlags = CONTEXT_FULL;
    if (GetThreadContext(th, &ctx)) {
        std::fprintf(f, "=== thread %lu\n", tid);
        STACKFRAME64 frame = {};
        frame.AddrPC.Offset = ctx.Rip;
        frame.AddrPC.Mode = AddrModeFlat;
        frame.AddrFrame.Offset = ctx.Rbp;
        frame.AddrFrame.Mode = AddrModeFlat;
        frame.AddrStack.Offset = ctx.Rsp;
        frame.AddrStack.Mode = AddrModeFlat;
        alignas(SYMBOL_INFO) char buf[sizeof(SYMBOL_INFO) + 512];
        for (int i = 0; i < 48; ++i) {
            if (!StackWalk64(IMAGE_FILE_MACHINE_AMD64, process, th, &frame, &ctx, nullptr,
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
    }
    ResumeThread(th);
    CloseHandle(th);
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
    for (DWORD tid : tids)
        quickDump(f, mods, tid, tid == g_guiTid.load());
    std::fprintf(f, "Pass 2 (dbghelp):\n");
    std::fflush(f);
    HANDLE process = GetCurrentProcess();
    SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS);
    SymInitialize(process, nullptr, TRUE);
    for (DWORD tid : tids)
        fullDump(f, process, tid);
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
