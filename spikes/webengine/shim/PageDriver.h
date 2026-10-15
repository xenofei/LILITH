#pragma once
// PageDriver: drives ComfyUI's own frontend (1.53.10) in QtWebEngine for the Image studio
// (IMG-127 Open in ComfyUI, IMG-129 Send to Worldwright, IMG-124 hidden conversion, ARCH-070
// lock-down). Exposed to QML as the singleton SpikeShim.PageDriver.
//
// The visible view is a QML WebEngineView (qml/WebDock.qml) using viewProfile, a named persistent
// QQuickWebEngineProfile whose storage lives under --profile-dir. PageDriver reaches it through the
// view's `run(id, script)` QML function, whose runJavaScript callback calls deliver(id, result).
// The hidden conversion page is a C++ QWebEnginePage that is never attached to a view, with its own
// off-the-record QWebEngineProfile.
//
// Page scripts are the files in js/ (embedded at :/js/). Each file is one JavaScript function
// expression; PageDriver appends the call with string arguments that must match
// [A-Za-z0-9+/=:._-]* (base64, request ids, URLs). Graph JSON never becomes script text: it is
// passed as base64 and decoded inside the page.

#include "spike_shim_global.h"

#include <QHash>
#include <QObject>
#include <QSet>
#include <QUrl>
#include <QVariant>
#include <QtWebEngineQuick/qquickwebengineprofile.h>

#include <functional>

class QWebEngineDownloadRequest;
class QWebEnginePage;
class QWebEngineProfile;

class SPIKE_SHIM_EXPORT PageDriver : public QObject
{
    Q_OBJECT
    Q_PROPERTY(int downloadAttempts READ downloadAttempts NOTIFY downloadAttemptsChanged)
    Q_PROPERTY(QQuickWebEngineProfile *viewProfile READ viewProfile CONSTANT)
    Q_PROPERTY(QUrl origin READ origin CONSTANT)

public:
    // profileDir: root for the persistent view profile (storage/, cache/, downloads/).
    PageDriver(const QString &profileDir, const QUrl &origin, QObject *parent = nullptr);
    ~PageDriver() override;

    // ---- The API from the plan (Task 6) ----------------------------------------------------
    // Starts loading a UI-format graph into a NEW workflow tab of the visible view. Returns the
    // request id; the page sets window.__wwFlags[id] to "pending", then "ok" or "error:<msg>".
    Q_INVOKABLE QString loadGraph(const QString &base64Json);
    // JSON.stringify(app.graph.serialize()) of the visible view's current graph ("" on failure).
    Q_INVOKABLE QString readGraph();
    // Loads the graph in the hidden page and returns JSON.stringify((await app.graphToPrompt()).output)
    // ("" on failure). Blocks the caller (nested event loop) until done or 60 s.
    Q_INVOKABLE QString convertHidden(const QString &base64Json);
    // window.__wwFlags[name] in the visible view ("" if unset or the page did not answer in 1 s).
    Q_INVOKABLE QString pollFlag(const QString &name);
    int downloadAttempts() const { return m_downloadAttempts; }

    // ---- Plumbing for the QML view ----------------------------------------------------------
    Q_INVOKABLE void attachView(QObject *view);
    Q_INVOKABLE void deliver(int id, const QVariant &result);
    // Navigation pin: true only for the ComfyUI origin (and about:blank / same-origin blob:).
    Q_INVOKABLE bool allowNavigation(const QUrl &url) const;

    // ---- Test helpers (used by the check driver in Main.qml) ---------------------------------
    Q_INVOKABLE QVariant runInView(const QString &scriptName, const QStringList &args = {}, int timeoutMs = 3000);
    Q_INVOKABLE QVariant runInHidden(const QString &scriptName, const QStringList &args = {}, int timeoutMs = 3000);
    Q_INVOKABLE QString exportFromView(const QString &commandId, int timeoutMs);
    Q_INVOKABLE bool waitForDownloadAttempts(int atLeast, int timeoutMs);
    Q_INVOKABLE QVariantMap hiddenNavigationTest(const QString &url, int waitMs);
    Q_INVOKABLE void sleep(int ms);
    Q_INVOKABLE QString readTextFile(const QString &path) const;
    Q_INVOKABLE bool writeTextFile(const QString &path, const QString &text) const;
    Q_INVOKABLE QString utf8ToBase64(const QString &text) const;
    Q_INVOKABLE QString sha256Hex(const QString &text) const;

    QQuickWebEngineProfile *viewProfile() const { return m_viewProfile; }
    QUrl origin() const { return m_origin; }

Q_SIGNALS:
    void downloadAttemptsChanged();
    void delivered(int id);

private:
    QString compose(const QString &scriptName, const QStringList &args, bool *ok = nullptr) const;
    int fireView(const QString &script);
    QVariant runViewScript(const QString &script, int timeoutMs, bool *answered = nullptr);
    int fireHidden(const QString &script);
    QVariant runHiddenScript(const QString &script, int timeoutMs, bool *answered = nullptr);
    QVariant takeResult(int id, int timeoutMs, bool *answered);
    bool waitUntil(const std::function<bool()> &pred, int timeoutMs);
    void ensureHiddenPage();
    bool ensureHiddenReady(int timeoutMs);
    void blockDownload(QWebEngineDownloadRequest *download, const QString &profile);

    QUrl m_origin;
    QString m_profileDir;
    QQuickWebEngineProfile *m_viewProfile = nullptr;
    QObject *m_view = nullptr;
    QWebEngineProfile *m_hiddenProfile = nullptr;
    QWebEnginePage *m_hiddenPage = nullptr;
    int m_hiddenLoad = 0;          // 0 loading, 1 ok, -1 failed
    bool m_hiddenReadyLogged = false;
    qint64 m_hiddenCreatedMs = 0;
    int m_seq = 0;
    int m_loadSeq = 0;
    int m_convSeq = 0;
    int m_exportSeq = 0;
    QHash<int, QVariant> m_results;
    QSet<int> m_waiting;
    int m_downloadAttempts = 0;
};
