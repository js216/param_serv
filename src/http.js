// JS bridge for param_serv::Connection on emscripten/WASM builds.
// Provides synchronous HTTP via XMLHttpRequest.

mergeInto(LibraryManager.library, {
    gui_http_request: function(method_ptr, url_ptr, body_ptr, body_len, out_ptr, out_len) {
        var method = UTF8ToString(method_ptr);
        var url = UTF8ToString(url_ptr);
        var body = body_len > 0 ? UTF8ArrayToString(HEAPU8, body_ptr, body_len) : null;
        var xhr = new XMLHttpRequest();
        xhr.open(method, url, false);
        xhr.send(body);
        window._last_xhr = xhr;
        var resp = xhr.responseText || '';
        stringToUTF8(resp, out_ptr, out_len);
        return lengthBytesUTF8(resp);
    },

    gui_http_get_header: function(name_ptr, out_ptr, out_len) {
        var name = UTF8ToString(name_ptr);
        var val = (window._last_xhr && window._last_xhr.getResponseHeader(name)) || '';
        stringToUTF8(val, out_ptr, out_len);
        return lengthBytesUTF8(val);
    },

    gui_get_server_url: function(out_ptr, out_len) {
        var url = window._param_serv_url || 'http://127.0.0.1:7777';
        stringToUTF8(url, out_ptr, out_len);
        return lengthBytesUTF8(url);
    },

    gui_set_led: function(name_ptr, value) {
        if (!window._led_state) window._led_state = {};
        var name = UTF8ToString(name_ptr);
        window._led_state[name] = value ? '1' : '0';
    },
});
