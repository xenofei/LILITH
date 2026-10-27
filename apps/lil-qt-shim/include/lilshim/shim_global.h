#pragma once
// The shim DLL's export macro (APP-030: what cxx-qt can't do lives in the C++ shim).

#include <QtCore/qglobal.h>

#if defined(LILSHIM_BUILD)
#  define LILSHIM_EXPORT Q_DECL_EXPORT
#else
#  define LILSHIM_EXPORT Q_DECL_IMPORT
#endif
