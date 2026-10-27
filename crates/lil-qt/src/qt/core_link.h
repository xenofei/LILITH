#pragma once
// How the list models (src/qt/json_list_model.rs) reach their `core`: any object with Core's
// API (APP-028), the Core singleton or the QML tests' FakeCore, reached through its meta-object.
// Its methods take QString arguments (Core) or untyped ones (a QML object's functions), and the
// link calls them either way. Its request and event signals are connected to the model's methods
// of the same shape: coreCallFinished, coreCallFailed, coreCallPending and coreEventReceived,
// whichever the model has. The core is held weakly: once it is gone, every call is refused.

#include <QtCore/QList>
#include <QtCore/QMetaObject>
#include <QtCore/QObject>
#include <QtCore/QPointer>
#include <QtCore/QString>
#include <QtCore/QVariant>

#include <memory>

class JsonListModel;
class EventLogModel;

namespace lil_qt {

class CoreLink
{
public:
    CoreLink() = default;
    CoreLink(const CoreLink &) = delete;
    CoreLink &operator=(const CoreLink &) = delete;
    ~CoreLink();

    // Drops the old core's connections, then binds to the object `core` holds (none for null or
    // a non-object): true when it holds one. (Const, as Rust holds the link in the model it binds.)
    bool bind(const QObject *model, const QVariant &core) const;
    QObject *core() const { return m_core.data(); }

private:
    void unbind() const;

    mutable QPointer<QObject> m_core;
    mutable QList<QMetaObject::Connection> m_connections;
};

std::unique_ptr<CoreLink> newCoreLink();

bool bindListModel(const CoreLink &link, const JsonListModel &model, const QVariant &core);
bool bindLogModel(const CoreLink &link, const EventLogModel &model, const QVariant &core);
// Whether the link has a core that is still there.
bool coreBound(const CoreLink &link);

// core.call(tool, argsJson): the request id, or empty without a core.
QString coreCall(const CoreLink &link, const QString &tool, const QString &argsJson);
// core.cancel(id), core.subscribe(topic), core.unsubscribe(topic): nothing without a core.
void coreCancel(const CoreLink &link, const QString &id);
void coreSubscribe(const CoreLink &link, const QString &topic);
void coreUnsubscribe(const CoreLink &link, const QString &topic);

} // namespace lil_qt
