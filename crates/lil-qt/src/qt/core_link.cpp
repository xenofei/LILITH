#include "lil_qt/src/qt/core_link.h"

#include "lil_qt/src/qt/json_list_model.cxxqt.h"

#include <QtCore/QMetaMethod>

namespace lil_qt {

namespace {

// The signals a model can follow, with the method each goes to.
constexpr const char *kRoutes[][2] = {
    {"callFinished", "coreCallFinished(QString,QString)"},
    {"callFailed", "coreCallFailed(QString,QString)"},
    {"callPendingConfirmation", "coreCallPending(QString,QString)"},
    {"coreEvent", "coreEventReceived(QString,QString,double,QString)"},
};

QMetaMethod findMethod(const QMetaObject *meta, const char *name, int parameters, QMetaMethod::MethodType type)
{
    for (int i = meta->methodCount() - 1; i >= 0; --i) {
        const QMetaMethod m = meta->method(i);
        if (m.methodType() == type && m.parameterCount() == parameters && m.name() == name)
            return m;
    }
    return {};
}

// Calls `name` on `object` with `args`, as QStrings or QVariants, whichever its parameters are;
// its return value as a QVariant (invalid for none, or when there is no such method).
QVariant invoke(QObject *object, const char *name, const QList<QString> &args)
{
    if (!object)
        return {};
    const QMetaObject *meta = object->metaObject();
    QMetaMethod m = findMethod(meta, name, int(args.size()), QMetaMethod::Method);
    if (!m.isValid())
        m = findMethod(meta, name, int(args.size()), QMetaMethod::Slot);
    if (!m.isValid())
        return {};
    QList<QVariant> variants;
    variants.reserve(args.size());
    QList<void *> argv{nullptr};
    for (int i = 0; i < args.size(); ++i) {
        const QMetaType type = m.parameterMetaType(i);
        if (type == QMetaType::fromType<QString>()) {
            argv.append(const_cast<QString *>(&args[i]));
        } else if (type == QMetaType::fromType<QVariant>()) {
            variants.append(QVariant(args[i]));
            argv.append(&variants.last());
        } else {
            return {};
        }
    }
    const QMetaType returnType = m.returnMetaType();
    QVariant result;
    if (returnType == QMetaType::fromType<QVariant>()) {
        // A QML function's: its value is written into the QVariant itself.
        argv[0] = &result;
    } else if (returnType.isValid() && returnType.id() != QMetaType::Void) {
        result = QVariant(returnType);
        argv[0] = result.data();
    }
    QMetaObject::metacall(object, QMetaObject::InvokeMetaMethod, m.methodIndex(), argv.data());
    return result;
}

} // namespace

CoreLink::~CoreLink()
{
    unbind();
}

void CoreLink::unbind() const
{
    for (const QMetaObject::Connection &c : std::as_const(m_connections))
        QObject::disconnect(c);
    m_connections.clear();
    m_core = nullptr;
}

bool CoreLink::bind(const QObject *model, const QVariant &core) const
{
    unbind();
    QObject *object = core.metaType().flags().testFlag(QMetaType::PointerToQObject) ? core.value<QObject *>() : nullptr;
    if (!object)
        return false;
    m_core = object;
    const QMetaObject *coreMeta = object->metaObject();
    const QMetaObject *modelMeta = model->metaObject();
    for (const auto &route : kRoutes) {
        const int slot = modelMeta->indexOfMethod(route[1]);
        if (slot < 0)
            continue;
        const QMetaMethod target = modelMeta->method(slot);
        const QMetaMethod signal = findMethod(coreMeta, route[0], target.parameterCount(), QMetaMethod::Signal);
        if (signal.isValid())
            m_connections.append(QObject::connect(object, signal, model, target));
    }
    return true;
}

std::unique_ptr<CoreLink> newCoreLink()
{
    return std::make_unique<CoreLink>();
}

bool bindListModel(const CoreLink &link, const JsonListModel &model, const QVariant &core)
{
    return link.bind(&model, core);
}

bool bindLogModel(const CoreLink &link, const EventLogModel &model, const QVariant &core)
{
    return link.bind(&model, core);
}

bool coreBound(const CoreLink &link)
{
    return link.core() != nullptr;
}

QString coreCall(const CoreLink &link, const QString &tool, const QString &argsJson)
{
    return invoke(link.core(), "call", {tool, argsJson}).toString();
}

void coreCancel(const CoreLink &link, const QString &id)
{
    invoke(link.core(), "cancel", {id});
}

void coreSubscribe(const CoreLink &link, const QString &topic)
{
    invoke(link.core(), "subscribe", {topic});
}

void coreUnsubscribe(const CoreLink &link, const QString &topic)
{
    invoke(link.core(), "unsubscribe", {topic});
}

} // namespace lil_qt
