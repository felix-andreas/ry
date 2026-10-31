#ifndef R_EXT_BOOLEAN_H_
#define R_EXT_BOOLEAN_H_

#undef FALSE
#undef TRUE

#ifdef __cplusplus
extern "C" {
#endif

typedef enum { FALSE = 0, TRUE } Rboolean;

#ifdef __cplusplus
}
#endif

#endif
