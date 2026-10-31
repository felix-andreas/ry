/* Connections at the C level: the struct behind every R connection object, and the four
   functions packages use to create custom connections and to read and write any connection.
   ry's implementation is in crates/libr/src/connections.rs.

   The struct is ABI: packages such as curl and processx allocate nothing themselves but fill in
   the method pointers of the struct that R_new_custom_connection hands back, and readers such as
   vroom call those pointers directly. Its member order, types and sizes therefore match GNU R's
   exactly; crates/libr/tests/test_connections.rs checks every offset against GNU R's header. */
#ifndef R_EXT_CONNECTIONS_H_
#define R_EXT_CONNECTIONS_H_

#include <R_ext/Boolean.h>

#if defined(__cplusplus) && !defined(DO_NOT_USE_CXX_HEADERS)
# include <cstddef>
# include <cstdarg>
#else
# include <stddef.h>
# include <stdarg.h>
#endif

/* The version of this interface. A package compares it at compile time and refuses to build
   against a layout it does not know. */
#define R_CONNECTIONS_VERSION 1

#ifndef HAVE_RCONNECTION_TYPEDEF
typedef struct Rconn *Rconnection;
#define HAVE_RCONNECTION_TYPEDEF
#endif

struct Rconn {
    /* The class ("file", ...) and what was opened; the runtime frees both on destroy. */
    char *class;
    char *description;
    /* The cetype_t encoding of the description. */
    int enc;
    /* The fopen-style mode, such as "r", "wb" or "a+". */
    char mode[5];
    Rboolean text, isopen, incomplete, canread, canwrite, canseek, blocking, isGzcon;

    Rboolean (*open)(struct Rconn *);
    /* Closes the device but keeps the connection, as after an automatic open. */
    void (*close)(struct Rconn *);
    /* Releases the connection's private state when its slot is freed. */
    void (*destroy)(struct Rconn *);
    int (*vfprintf)(struct Rconn *, const char *, va_list);
    int (*fgetc)(struct Rconn *);
    int (*fgetc_internal)(struct Rconn *);
    /* Returns the old position; origin 1 start, 2 current, 3 end; rw 1 read, 2 write. */
    double (*seek)(struct Rconn *, double, int, int);
    void (*truncate)(struct Rconn *);
    int (*fflush)(struct Rconn *);
    size_t (*read)(void *, size_t, size_t, struct Rconn *);
    size_t (*write)(const void *, size_t, size_t, struct Rconn *);

    /* pushBack() lines, read before the device: the last first, from byte posPushBack. */
    int nPushBack, posPushBack;
    char **PushBack;
    /* One character of look-ahead, kept while splitting lines at "\r" and "\r\n". */
    int save, save2;

    /* Re-encoding state. */
    char encname[101];
    void *inconv, *outconv;
    char iconvbuff[25], oconvbuff[50], *next, init_out[25];
    short navail, inavail;
    Rboolean EOF_signalled;
    Rboolean UTF8out;

    void *id;
    /* The external pointer the R-level connection object carries as "conn_id". */
    void *ex_ptr;
    /* The class's own state; a custom connection's belongs to its package. */
    void *private;
    /* The exit status of a closed pipe. */
    int status;

    /* The read buffer of a text-mode connection. */
    unsigned char *buff;
    size_t buff_len, buff_stored_len, buff_pos;
};

#ifdef __cplusplus
extern "C" {
#endif

/* Creates a connection of class `class_name`, with methods that each report they are not
   enabled, stores its struct in `*ptr` for the caller to fill in, and returns the R object. */
SEXP R_new_custom_connection(const char *description, const char *mode, const char *class_name,
                             Rconnection *ptr);
/* Read or write `n` bytes through the connection's methods, returning the count transferred.
   Both raise an R error when the connection is not open or not open in that direction. */
size_t R_ReadConnection(Rconnection con, void *buf, size_t n);
size_t R_WriteConnection(Rconnection con, void *buf, size_t n);
/* The struct behind an R connection object, or an R error if `sConn` is not a live one. */
Rconnection R_GetConnection(SEXP sConn);

#ifdef __cplusplus
}
#endif

#endif
