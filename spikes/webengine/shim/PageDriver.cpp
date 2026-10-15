#include "PageDriver.h"
#include "SpikeLog.h"

#include <QCryptographicHash>
#include <QDir>
#include <QElapsedTimer>
#include <QEventLoop>
#include <QFile>
#include <QRegularExpression>
#include <QTimer>
#include <QWebEngineDownloadRequest>
#include <QWebEnginePage>
#include <QWebEngineProfile>
#include <QWebEngineSettings>
#include <QtWebEngineQuick/qquickwebenginedownloadrequest.h>

#include <algorithm>

namespace {

QString downloadStateName(QWebEngineDownloadRequest::DownloadState s)
{
    switch (s) {
    case QWebEngineDownloadRequest::DownloadRequested: return QStringLiteral("requested");
    case QWebEngineDownloadRequest::DownloadInProgress: return QStringLiteral("in_progress");
    case QWebEngineDownloadRequest::DownloadCompleted: return QStringLiteral("completed");
    case QWebEngineDownloadRequest::DownloadCancelled: return QStringLiteral("cancelled");
    case QWebEngineDownloadRequest::DownloadInterrupted: return QStringLiteral("interrupted");
    }
    return QStringLiteral("unknown");
}

// The hidden conversion page: never attached to a view. acceptNavigationRequest pins it to the
// ComfyUI origin; console warnings/errors go to the log (they are the only window into it).
class HiddenPage : public QWebEnginePage
{
public:
    HiddenPage(QWebEngineProfile *profile, PageDriver *driver)
        : QWebEnginePage(profile, driver), m_driver(driver) {}

protected:
    bool acceptNavigationRequest(const QUrl &url, NavigationType type, bool isMainFrame) override
    {
        const bool allowed = m_driver->allowNavigation(url);
        if (!allowed || isMainFrame)
            SpikeLog::write(QStringLiteral("hidden_navigation"),
                            {{"url", url.toString()}, {"type", int(type)}, {"main_frame", isMainFrame}, {"allowed", allowed}});
        return allowed;
    }

    void javaScriptConsoleMessage(JavaScriptConsoleMessageLevel level, const QString &message, int line,
                                  const QString &source) override
    {
        if (level == InfoMessageLevel || m_logged >= 60)
            return;
        ++m_logged;
        SpikeLog::write(QStringLiteral("hidden_console"),
                        {{"level", level == WarningMessageLevel ? "warning" : "error"},
                         {"message", message.left(400)}, {"line", line}, {"source", source.section(QLatin1Char('/'), -1)}});
    }

private:
    PageDriver *m_driver;
    int m_logged = 0;
};

const QRegularExpression &argPattern()
{
    // anchoredPattern wraps it as \A(?:...)\z; a plain ^...$ also accepts one trailing "\n".
    static const QRegularExpression re(QRegularExpression::anchoredPattern(QStringLiteral("[A-Za-z0-9+/=:._\\-]*")));
    return re;
}

} // namespace

PageDriver::PageDriver(const QString &profileDir, const QUrl &origin, QObject *parent)
    : QObject(parent), m_origin(origin), m_profileDir(profileDir)
{
    // Named persistent profile for the visible ComfyUI view (ARCH-070 / IMG-126). Everything it
    // stores (local storage, IndexedDB, cookies, cache) is under profileDir, not in AppData.
    // The paths are set before the storage name: constructing with the name first makes WebEngine
    // create its default folder (%APPDATA%\<app>\QtWebEngine\<name>) before it is redirected.
    QDir().mkpath(profileDir);
    m_viewProfile = new QQuickWebEngineProfile(this);
    m_viewProfile->setPersistentStoragePath(QDir(profileDir).filePath(QStringLiteral("storage")));
    m_viewProfile->setCachePath(QDir(profileDir).filePath(QStringLiteral("cache")));
    m_viewProfile->setDownloadPath(QDir(profileDir).filePath(QStringLiteral("downloads")));
    m_viewProfile->setStorageName(QStringLiteral("worldwright-spike-comfyui"));
    m_viewProfile->setOffTheRecord(false);
    connect(m_viewProfile, &QQuickWebEngineProfile::downloadRequested, this,
            [this](QQuickWebEngineDownloadRequest *d) { blockDownload(d, QStringLiteral("view")); });
    SpikeLog::write(QStringLiteral("view_profile"),
                    {{"storage_name", m_viewProfile->storageName()},
                     {"off_the_record", m_viewProfile->isOffTheRecord()},
                     {"persistent_storage_path", m_viewProfile->persistentStoragePath()},
                     {"cache_path", m_viewProfile->cachePath()},
                     {"download_path", m_viewProfile->downloadPath()}});
}

PageDriver::~PageDriver()
{
    // A page must go before its profile.
    delete m_hiddenPage;
    m_hiddenPage = nullptr;
    delete m_hiddenProfile;
    m_hiddenProfile = nullptr;
}

// ---------------------------------------------------------------------------------------------
// Script plumbing
// ---------------------------------------------------------------------------------------------

QString PageDriver::compose(const QString &scriptName, const QStringList &args, bool *ok) const
{
    if (ok)
        *ok = false;
    static const QRegularExpression nameRe(QRegularExpression::anchoredPattern(QStringLiteral("[a-z_]+\\.js")));
    if (!nameRe.match(scriptName).hasMatch()) {
        SpikeLog::write(QStringLiteral("driver_error"), {{"what", "bad script name"}, {"name", scriptName}});
        return {};
    }
    QFile f(QStringLiteral(":/js/") + scriptName);
    if (!f.open(QIODevice::ReadOnly)) {
        SpikeLog::write(QStringLiteral("driver_error"), {{"what", "script not found"}, {"name", scriptName}});
        return {};
    }
    const QString body = QString::fromUtf8(f.readAll());
    QStringList quoted;
    for (int i = 0; i < args.size(); ++i) {
        const QString &a = args.at(i);
        // Only characters that cannot end a double-quoted JS string literal.
        if (!argPattern().match(a).hasMatch()) {
            SpikeLog::write(QStringLiteral("driver_error"),
                            {{"what", "argument rejected"}, {"script", scriptName}, {"arg_index", i}, {"arg_chars", a.size()}});
            return {};
        }
        quoted << QLatin1Char('"') + a + QLatin1Char('"');
    }
    if (ok)
        *ok = true;
    return QStringLiteral("(\n") + body + QStringLiteral("\n)(") + quoted.join(QLatin1Char(',')) + QStringLiteral(");");
}

void PageDriver::attachView(QObject *view)
{
    m_view = view;
    SpikeLog::write(QStringLiteral("view_attached"), {{"ok", view != nullptr}});
}

void PageDriver::deliver(int id, const QVariant &result)
{
    if (!m_waiting.remove(id))
        return; // nobody is waiting (fire-and-forget call, or the wait timed out)
    m_results.insert(id, result);
    emit delivered(id);
}

int PageDriver::fireView(const QString &script)
{
    if (!m_view || script.isEmpty())
        return 0;
    const int id = ++m_seq;
    QMetaObject::invokeMethod(m_view, "run", Q_ARG(QVariant, id), Q_ARG(QVariant, script));
    return id;
}

int PageDriver::fireHidden(const QString &script)
{
    if (!m_hiddenPage || script.isEmpty())
        return 0;
    const int id = ++m_seq;
    m_hiddenPage->runJavaScript(script, [this, id](const QVariant &v) { deliver(id, v); });
    return id;
}

bool PageDriver::waitUntil(const std::function<bool()> &pred, int timeoutMs)
{
    QElapsedTimer t;
    t.start();
    while (!pred()) {
        const qint64 left = timeoutMs - t.elapsed();
        if (left <= 0)
            return false;
        QEventLoop loop;
        QTimer timer;
        timer.setSingleShot(true);
        connect(&timer, &QTimer::timeout, &loop, &QEventLoop::quit);
        connect(this, &PageDriver::delivered, &loop, &QEventLoop::quit);
        timer.start(int(std::min<qint64>(left, 20)));
        loop.exec();
    }
    return true;
}

QVariant PageDriver::takeResult(int id, int timeoutMs, bool *answered)
{
    const bool got = id != 0 && waitUntil([&] { return m_results.contains(id); }, timeoutMs);
    m_waiting.remove(id);
    if (answered)
        *answered = got;
    return got ? m_results.take(id) : QVariant();
}

QVariant PageDriver::runViewScript(const QString &script, int timeoutMs, bool *answered)
{
    const int next = m_seq + 1;
    m_waiting.insert(next);
    const int id = fireView(script);
    if (id != next)
        m_waiting.remove(next);
    return takeResult(id, timeoutMs, answered);
}

QVariant PageDriver::runHiddenScript(const QString &script, int timeoutMs, bool *answered)
{
    const int next = m_seq + 1;
    m_waiting.insert(next);
    const int id = fireHidden(script);
    if (id != next)
        m_waiting.remove(next);
    return takeResult(id, timeoutMs, answered);
}

QVariant PageDriver::runInView(const QString &scriptName, const QStringList &args, int timeoutMs)
{
    return runViewScript(compose(scriptName, args), timeoutMs);
}

QVariant PageDriver::runInHidden(const QString &scriptName, const QStringList &args, int timeoutMs)
{
    ensureHiddenPage();
    return runHiddenScript(compose(scriptName, args), timeoutMs);
}

// ---------------------------------------------------------------------------------------------
// The plan's API
// ---------------------------------------------------------------------------------------------

QString PageDriver::loadGraph(const QString &base64Json)
{
    const QString id = QStringLiteral("ww-load-%1").arg(++m_loadSeq);
    bool ok = false;
    const QString script = compose(QStringLiteral("load_graph.js"), {base64Json, id}, &ok);
    const int callId = ok ? fireView(script) : 0;
    SpikeLog::write(QStringLiteral("load_graph_call"),
                    {{"request_id", id}, {"base64_chars", base64Json.size()}, {"script_ok", ok},
                     {"sent", callId != 0}, {"script_chars", script.size()}});
    return id;
}

QString PageDriver::readGraph()
{
    bool answered = false;
    const QVariant v = runViewScript(compose(QStringLiteral("read_graph.js"), {}), 5000, &answered);
    if (!answered || v.typeId() != QMetaType::QString)
        SpikeLog::write(QStringLiteral("read_graph_failed"), {{"answered", answered}, {"type", QString::fromLatin1(v.typeName())}});
    return v.typeId() == QMetaType::QString ? v.toString() : QString();
}

QString PageDriver::pollFlag(const QString &name)
{
    const QVariant v = runViewScript(compose(QStringLiteral("poll_flag.js"), {name}), 1000);
    return v.typeId() == QMetaType::QString ? v.toString() : QString();
}

void PageDriver::ensureHiddenPage()
{
    if (m_hiddenPage)
        return;
    m_hiddenCreatedMs = SpikeLog::instance()->elapsedMs();
    // No storage name: an off-the-record profile (memory only), separate from the view's profile.
    m_hiddenProfile = new QWebEngineProfile(this);
    m_hiddenProfile->setDownloadPath(QDir(m_profileDir).filePath(QStringLiteral("downloads")));
    connect(m_hiddenProfile, &QWebEngineProfile::downloadRequested, this,
            [this](QWebEngineDownloadRequest *d) { blockDownload(d, QStringLiteral("hidden")); });
    m_hiddenPage = new HiddenPage(m_hiddenProfile, this);
    connect(m_hiddenPage, &QWebEnginePage::loadFinished, this, [this](bool ok) {
        m_hiddenLoad = ok ? 1 : -1;
        SpikeLog::write(QStringLiteral("hidden_load_finished"),
                        {{"ok", ok}, {"url", m_hiddenPage->url().toString()},
                         {"ms_since_created", SpikeLog::instance()->elapsedMs() - m_hiddenCreatedMs}});
    });
    m_hiddenLoad = 0;
    m_hiddenPage->load(m_origin);
    SpikeLog::write(QStringLiteral("hidden_page_created"),
                    {{"off_the_record", m_hiddenProfile->isOffTheRecord()},
                     {"storage_name", m_hiddenProfile->storageName()},
                     {"persistent_storage_path", m_hiddenProfile->persistentStoragePath()},
                     {"http_cache_type", int(m_hiddenProfile->httpCacheType())},
                     {"has_view", false},
                     {"visible", m_hiddenPage->isVisible()},
                     {"lifecycle_state", int(m_hiddenPage->lifecycleState())}});
}

bool PageDriver::ensureHiddenReady(int timeoutMs)
{
    ensureHiddenPage();
    QElapsedTimer t;
    t.start();
    if (!waitUntil([this] { return m_hiddenLoad != 0; }, timeoutMs) || m_hiddenLoad < 0)
        return false;
    bool ready = false;
    waitUntil([&] {
        ready = runHiddenScript(compose(QStringLiteral("app_ready.js"), {}), 1000).toBool();
        return ready;
    }, int(std::max<qint64>(0, timeoutMs - t.elapsed())));
    if (ready && !m_hiddenReadyLogged) {
        m_hiddenReadyLogged = true;
        SpikeLog::write(QStringLiteral("hidden_page_ready"),
                        {{"ms_since_created", SpikeLog::instance()->elapsedMs() - m_hiddenCreatedMs},
                         {"visible", m_hiddenPage->isVisible()},
                         {"lifecycle_state", int(m_hiddenPage->lifecycleState())}});
    }
    return ready;
}

QString PageDriver::convertHidden(const QString &base64Json)
{
    QElapsedTimer t;
    t.start();
    if (!ensureHiddenReady(60000)) {
        SpikeLog::write(QStringLiteral("convert_hidden_error"),
                        {{"what", "hidden page not ready"}, {"load_state", m_hiddenLoad}, {"ms", t.elapsed()}});
        return {};
    }
    const QString id = QStringLiteral("ww-conv-%1").arg(++m_convSeq);
    bool ok = false;
    const QString script = compose(QStringLiteral("convert.js"), {base64Json, id}, &ok);
    if (!ok || !fireHidden(script))
        return {};
    QString flag;
    const QString poll = compose(QStringLiteral("poll_flag.js"), {id});
    waitUntil([&] {
        flag = runHiddenScript(poll, 2000).toString();
        return flag == QLatin1String("ok") || flag.startsWith(QLatin1String("error:"));
    }, int(std::max<qint64>(1000, 60000 - t.elapsed())));
    if (flag != QLatin1String("ok")) {
        SpikeLog::write(QStringLiteral("convert_hidden_error"), {{"request_id", id}, {"flag", flag}, {"ms", t.elapsed()}});
        return {};
    }
    const QVariant v = runHiddenScript(compose(QStringLiteral("take_result.js"), {id}), 5000);
    return v.typeId() == QMetaType::QString ? v.toString() : QString();
}

// ---------------------------------------------------------------------------------------------
// Lock-down: navigation pin and download block
// ---------------------------------------------------------------------------------------------

bool PageDriver::allowNavigation(const QUrl &url) const
{
    const auto sameOrigin = [this](const QUrl &u) {
        return u.scheme() == m_origin.scheme() && u.host() == m_origin.host() &&
               u.port(u.scheme() == QLatin1String("https") ? 443 : 80) == m_origin.port(80);
    };
    if (sameOrigin(url))
        return true;
    if (url.scheme() == QLatin1String("about"))
        return url.path() == QLatin1String("blank") || url.path() == QLatin1String("srcdoc");
    if (url.scheme() == QLatin1String("blob"))
        return sameOrigin(QUrl(url.path()));
    return false;
}

void PageDriver::blockDownload(QWebEngineDownloadRequest *download, const QString &profile)
{
    ++m_downloadAttempts;
    const QString before = downloadStateName(download->state());
    // ARCH-070 / IMG-126: no file is ever saved from the page.
    download->cancel();
    SpikeLog::write(QStringLiteral("download_blocked"),
                    {{"profile", profile},
                     {"suggested_file_name", download->suggestedFileName()},
                     {"mime_type", download->mimeType()},
                     {"url_scheme", download->url().scheme()},
                     {"url_origin", download->url().scheme() == QLatin1String("blob") ? download->url().path().section(QLatin1Char('/'), 0, 2)
                                                                                       : download->url().host()},
                     {"state_before", before},
                     {"state_after_cancel", downloadStateName(download->state())},
                     {"attempts", m_downloadAttempts}});
    emit downloadAttemptsChanged();
}

// ---------------------------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------------------------

QString PageDriver::exportFromView(const QString &commandId, int timeoutMs)
{
    QElapsedTimer t;
    t.start();
    const QString id = QStringLiteral("ww-export-%1").arg(++m_exportSeq);
    runViewScript(compose(QStringLiteral("export_capture.js"), {id, commandId}), 3000);
    QString flag;
    waitUntil([&] {
        flag = pollFlag(id);
        return flag == QLatin1String("ok") || flag.startsWith(QLatin1String("error:"));
    }, timeoutMs);
    if (flag != QLatin1String("ok")) {
        SpikeLog::write(QStringLiteral("export_capture_failed"), {{"command", commandId}, {"flag", flag}, {"ms", t.elapsed()}});
        return {};
    }
    const QVariant v = runViewScript(compose(QStringLiteral("take_result.js"), {id}), 3000);
    return v.typeId() == QMetaType::QString ? v.toString() : QString();
}

bool PageDriver::waitForDownloadAttempts(int atLeast, int timeoutMs)
{
    return waitUntil([&] { return m_downloadAttempts >= atLeast; }, timeoutMs);
}

QVariantMap PageDriver::hiddenNavigationTest(const QString &url, int waitMs)
{
    QVariantMap r;
    r.insert(QStringLiteral("target"), url);
    if (!ensureHiddenReady(60000)) {
        r.insert(QStringLiteral("error"), QStringLiteral("hidden page not ready"));
        return r;
    }
    r.insert(QStringLiteral("url_before"), m_hiddenPage->url().toString());
    r.insert(QStringLiteral("probe"), runHiddenScript(compose(QStringLiteral("nav_probe.js"), {url, QStringLiteral("href")}), 2000));
    sleep(waitMs);
    r.insert(QStringLiteral("url_after"), m_hiddenPage->url().toString());
    r.insert(QStringLiteral("page_origin_after"),
             runHiddenScript(compose(QStringLiteral("nav_probe.js"), {QString(), QStringLiteral("origin")}), 2000));
    r.insert(QStringLiteral("app_alive_after"), runHiddenScript(compose(QStringLiteral("app_ready.js"), {}), 2000));
    return r;
}

void PageDriver::sleep(int ms)
{
    QEventLoop loop;
    QTimer::singleShot(ms, &loop, &QEventLoop::quit);
    loop.exec();
}

QString PageDriver::readTextFile(const QString &path) const
{
    QFile f(path);
    return f.open(QIODevice::ReadOnly) ? QString::fromUtf8(f.readAll()) : QString();
}

bool PageDriver::writeTextFile(const QString &path, const QString &text) const
{
    QFile f(path);
    if (!f.open(QIODevice::WriteOnly | QIODevice::Truncate))
        return false;
    return f.write(text.toUtf8()) >= 0;
}

QString PageDriver::utf8ToBase64(const QString &text) const
{
    return QString::fromLatin1(text.toUtf8().toBase64());
}

QString PageDriver::sha256Hex(const QString &text) const
{
    return QString::fromLatin1(QCryptographicHash::hash(text.toUtf8(), QCryptographicHash::Sha256).toHex());
}
