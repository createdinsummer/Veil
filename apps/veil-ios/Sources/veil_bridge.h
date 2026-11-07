#ifndef VEIL_BRIDGE_H
#define VEIL_BRIDGE_H
#include <stddef.h>
#include <stdint.h>

typedef struct VeilService VeilService;
typedef struct { uint8_t *data; size_t length; uint32_t kind; } VeilBuffer;

_Bool veil_configure_app_data_dir(const char *path);
VeilService *veil_client_new(void);
void veil_client_free(VeilService *client);
_Bool veil_client_cancel_transfer(const VeilService *client);
VeilBuffer veil_client_progress(const VeilService *client);
VeilBuffer veil_client_call(VeilService *client, const uint8_t *data, size_t length);
void veil_log_client_error(uint32_t category, const char *source, uint32_t line, int64_t native_code, const char *message);
void veil_logs_flush(void);
VeilBuffer veil_logs_export(void);
void veil_buffer_free(VeilBuffer buffer);
#endif
