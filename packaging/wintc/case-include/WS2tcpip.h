/* Case alias for a Linux sysroot; also drops rpc.h's `#define interface struct`,
   which clashes with Ableton Link's variable names. */
#include <ws2tcpip.h>
#undef interface
