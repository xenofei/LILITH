#include "lilshim/Fonts.h"

#include "lilshim/LogBridge.h"

#include <QFont>
#include <QFontDatabase>
#include <QFontInfo>
#include <QGuiApplication>
#include <QJsonArray>
#include <QLoggingCategory>
#include <QStringList>

#include <iterator>

Q_LOGGING_CATEGORY(lcFonts, "lil.shim.fonts")

namespace lilshim {

namespace {

const char *const kFaces[] = {"Inter-Regular.ttf", "Inter-Bold.ttf"};

void fail(const QString &family, const QString &why, const QJsonArray &failed)
{
    qCWarning(lcFonts) << "the UI font" << family << "did not load:" << why << "- the system UI font is used";
    logEvent(QStringLiteral("error"), {{"what", "the UI font did not load"},
                                       {"family", family},
                                       {"why", why},
                                       {"failed", failed}});
}

} // namespace

bool applyThemeFont(const QString &family)
{
    QStringList families;
    QJsonArray failed;
    for (const char *face : kFaces) {
        const QString path = QStringLiteral(":/lilith/fonts/inter/") + QString::fromLatin1(face);
        const int id = QFontDatabase::addApplicationFont(path);
        if (id < 0)
            failed.append(QString::fromLatin1(face));
        else
            families << QFontDatabase::applicationFontFamilies(id);
    }
    if (!failed.isEmpty() || !families.contains(family)) {
        fail(family, failed.isEmpty() ? QStringLiteral("the bundled faces are another family")
                                      : QStringLiteral("a bundled face failed to load"),
             failed);
        return false;
    }
    const QFont before = QGuiApplication::font();
    QFont font = before;
    font.setFamilies({family});
    QGuiApplication::setFont(font);
    const QString resolved = QFontInfo(QGuiApplication::font()).family();
    if (resolved != family) {
        QGuiApplication::setFont(before);
        fail(family, QStringLiteral("the application font resolved to ") + resolved, failed);
        return false;
    }
    logEvent(QStringLiteral("fonts_loaded"), {{"family", family}, {"faces", double(std::size(kFaces))}});
    return true;
}

} // namespace lilshim
