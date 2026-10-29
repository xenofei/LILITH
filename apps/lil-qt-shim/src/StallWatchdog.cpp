#include "lilshim/StallWatchdog.h"

#include "lilshim/LogBridge.h"

#include <QCoreApplication>
#include <QElapsedTimer>
#include <QFontDatabase>
#include <QJsonArray>
#include <QThreadPool>
#include <QTimer>

#include <algorithm>
#include <atomic>
#include <cstdio>
#include <thread>
#include <vector>

#ifndef NOMINMAX
#  define NOMINMAX
#endif
#ifndef WIN32_LEAN_AND_MEAN
#  define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>
#include <tlhelp32.h>

namespace lilshim {

namespace {

// A coarse beat: a 50 ms stall doesn't need a finer one, and a precise timer under 20 ms raises
// Windows' timer resolution for the life of the app.
constexpr int kBeatMs = 20;
constexpr SIZE_T kStackCopy = 32 * 1024;
constexpr int kMaxFrames = 32;

QElapsedTimer g_clock;
std::atomic<qint64> g_lastBeatMs{0};
std::atomic<bool> g_stop{false};
std::atomic<int> g_stalls{0};
std::atomic<int> g_stacks{0};
DWORD g_guiTid = 0;

struct Module
{
    DWORD64 base;
    DWORD64 size;
    wchar_t name[MAX_MODULE_NAME32 + 1];
};

std::vector<Module> modules()
{
    std::vector<Module> out;
    HANDLE snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE, GetCurrentProcessId());
    if (snap == INVALID_HANDLE_VALUE)
        return out;
    MODULEENTRY32W me{};
    me.dwSize = sizeof(me);
    if (Module32FirstW(snap, &me)) {
        do {
            Module m{DWORD64(me.modBaseAddr), DWORD64(me.modBaseSize), {}};
            wcsncpy_s(m.name, me.szModule, _TRUNCATE);
            out.push_back(m);
        } while (Module32NextW(snap, &me));
    }
    CloseHandle(snap);
    return out;
}

// The GUI thread's instruction pointer and the words of the top of its stack that point into a
// loaded module (likely return addresses). The thread is suspended only for GetThreadContext and
// one ReadProcessMemory, kernel calls that take no user-mode lock, and is resumed before anything
// is allocated or logged.
QJsonArray guiStack()
{
    std::vector<unsigned char> stack(kStackCopy);
    CONTEXT ctx{};
    SIZE_T copied = 0;
    bool ok = false;
    HANDLE th = OpenThread(THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT, FALSE, g_guiTid);
    if (!th)
        return {};
    if (SuspendThread(th) != DWORD(-1)) {
        ctx.ContextFlags = CONTEXT_CONTROL;
        if (GetThreadContext(th, &ctx)) {
            ok = true;
            // Page by page from Rsp, so the copy stops at the end of the committed stack.
            const DWORD64 page = 4096;
            DWORD64 at = ctx.Rsp;
            while (copied < kStackCopy) {
                const SIZE_T want = SIZE_T(std::min<DWORD64>(page - (at % page), kStackCopy - copied));
                SIZE_T got = 0;
                if (!ReadProcessMemory(GetCurrentProcess(), reinterpret_cast<LPCVOID>(at), stack.data() + copied, want, &got) || got == 0)
                    break;
                copied += got;
                at += got;
            }
        }
        ResumeThread(th);
    }
    CloseHandle(th);
    if (!ok)
        return {};
    const std::vector<Module> mods = modules();
    auto frame = [&mods](DWORD64 a) -> QString {
        for (const Module &m : mods) {
            if (a >= m.base && a < m.base + m.size)
                return QString::fromWCharArray(m.name) + QStringLiteral("+0x") + QString::number(a - m.base, 16);
        }
        return {};
    };
    QJsonArray frames;
    const QString rip = frame(ctx.Rip);
    frames.append(rip.isEmpty() ? QStringLiteral("?") : rip);
    const auto *words = reinterpret_cast<const DWORD64 *>(stack.data());
    for (SIZE_T i = 0; i < copied / sizeof(DWORD64) && frames.size() < kMaxFrames; ++i) {
        const QString f = frame(words[i]);
        if (!f.isEmpty())
            frames.append(f);
    }
    return frames;
}

void watch()
{
    qint64 dumpedFor = -1;
    qint64 lastDumpMs = -kDumpEveryMs;
    while (!g_stop.load()) {
        std::this_thread::sleep_for(std::chrono::milliseconds(25));
        const qint64 beat = g_lastBeatMs.load();
        const qint64 now = g_clock.elapsed();
        const qint64 stalled = now - beat;
        if (stalled > kStallDumpMs && dumpedFor != beat && now - lastDumpMs >= kDumpEveryMs) {
            dumpedFor = beat;
            lastDumpMs = now;
            logEvent(QStringLiteral("gui_stall_stack"), {{"ms", double(stalled)}, {"frames", guiStack()}});
            ++g_stacks;
        }
    }
}

} // namespace

void startStallWatchdog()
{
    if (g_guiTid)
        return;
    g_guiTid = GetCurrentThreadId();
    g_clock.start();
    g_lastBeatMs = g_clock.elapsed();
    auto *timer = new QTimer(QCoreApplication::instance());
    timer->setTimerType(Qt::CoarseTimer);
    timer->setInterval(kBeatMs);
    QObject::connect(timer, &QTimer::timeout, [] {
        const qint64 now = g_clock.elapsed();
        const qint64 late = now - g_lastBeatMs.exchange(now) - kBeatMs;
        if (late > kStallLogMs) {
            logEvent(QStringLiteral("gui_stall"), {{"ms", double(late)}});
            ++g_stalls;
        }
    });
    timer->start();
    std::thread watcher(watch);
    QObject::connect(QCoreApplication::instance(), &QCoreApplication::aboutToQuit, [] { g_stop = true; });
    watcher.detach();
}

StallCounts stallCounts()
{
    return {g_stalls.load(), g_stacks.load()};
}

void warmFonts()
{
    QThreadPool::globalInstance()->start([] {
        QElapsedTimer took;
        took.start();
        const qsizetype families = QFontDatabase::families().size();
        logEvent(QStringLiteral("fonts_warm"), {{"families", double(families)}, {"ms", double(took.elapsed())}});
    });
}

} // namespace lilshim
