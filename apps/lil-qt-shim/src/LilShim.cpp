#include "LilShim.h"

#include "lilshim/Activation.h"
#include "lilshim/StallWatchdog.h"

#include <QAccessible>
#include <QColor>
#include <QFont>
#include <QFontInfo>
#include <QRawFont>
#include <QGuiApplication>
#include <QPalette>

LilShim::LilShim(QObject *parent)
    : QObject(parent)
{
}

bool LilShim::isModuleLoaded(const QString &dll) const
{
    return lilshim::isModuleLoaded(dll);
}

void LilShim::showInExplorer(const QString &path) const
{
    lilshim::showInExplorer(path);
}

QVariantMap LilShim::accessibleOf(QObject *item) const
{
    QAccessibleInterface *iface = item ? QAccessible::queryAccessibleInterface(item) : nullptr;
    if (!iface)
        return {{"role", 0}, {"name", QString()}, {"focusable", false}, {"actions", QStringList()}};
    QAccessibleActionInterface *actions = iface->actionInterface();
    return {{"role", int(iface->role())},
            {"name", iface->text(QAccessible::Name)},
            {"focusable", bool(iface->state().focusable)},
            {"actions", actions ? actions->actionNames() : QStringList()}};
}

void LilShim::applyPalette(const QVariantMap &colors) const
{
    static const std::pair<const char *, QPalette::ColorRole> roles[] = {
        {"window", QPalette::Window},       {"windowText", QPalette::WindowText},
        {"base", QPalette::Base},           {"alternateBase", QPalette::AlternateBase},
        {"text", QPalette::Text},           {"button", QPalette::Button},
        {"buttonText", QPalette::ButtonText}, {"light", QPalette::Light},
        {"mid", QPalette::Mid},             {"dark", QPalette::Dark},
        {"highlight", QPalette::Highlight}, {"highlightedText", QPalette::HighlightedText},
        {"placeholderText", QPalette::PlaceholderText},
    };
    QPalette palette = QGuiApplication::palette();
    for (const auto &[key, role] : roles) {
        const QVariant v = colors.value(QString::fromLatin1(key));
        if (v.isValid())
            palette.setColor(role, v.value<QColor>());
    }
    QGuiApplication::setPalette(palette);
}

void LilShim::startStallWatchdog() const
{
    lilshim::startStallWatchdog();
}

QVariantMap LilShim::stallCounts() const
{
    const lilshim::StallCounts c = lilshim::stallCounts();
    return {{"stalls", c.stalls}, {"stacks", c.stacks}};
}

QVariantMap LilShim::resolvedFont(int weight) const
{
    QFont font = QGuiApplication::font();
    font.setWeight(QFont::Weight(weight));
    const QFontInfo info(font);
    // The face file the engine really uses: a weight synthesised from another face keeps that
    // face's style name.
    const QRawFont raw = QRawFont::fromFont(font);
    return {{"family", info.family()}, {"weight", info.weight()}, {"style", raw.styleName()}};
}
