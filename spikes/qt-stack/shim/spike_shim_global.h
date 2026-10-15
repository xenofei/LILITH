#pragma once
#include <QtCore/qglobal.h>

#if defined(SPIKE_SHIM_BUILD)
#  define SPIKE_SHIM_EXPORT Q_DECL_EXPORT
#else
#  define SPIKE_SHIM_EXPORT Q_DECL_IMPORT
#endif
