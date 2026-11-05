#ifndef VEIL_BRIDGE_H
#define VEIL_BRIDGE_H
#include <stddef.h>
#include <stdint.h>

typedef struct VeilService VeilService;
typedef struct { uint8_t *data; size_t length; uint32_t kind; } VeilBuffer;

_Bool veil_configure_app_data_dir(const char *path);
VeilService *veil_client_new(void);
void veil_client_free(VeilService *client);
VeilBuffer veil_client_call(VeilService *client, const uint8_t *data, size_t length);
void veil_buffer_free(VeilBuffer buffer);
#endif
