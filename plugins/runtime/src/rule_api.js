// Bridge JS promises to read-only host data queries.
((context) => {
    const waiting = new Map();
    let queued = [];
    let next = 0;
    function query(kind, args) {
        if (next >= 32) return Promise.reject(new Error("Too many host queries"));
        const id = ++next;
        return new Promise((resolve, reject) => {
            waiting.set(id, { resolve, reject });
            queued.push({ id, kind, args });
        });
    }
    return {
        host: Object.freeze({
            context: Object.freeze(context),
            exists: path => query("exists", { path }),
            readFile: (path, options = {}) => query("read_file", { ...options, path }),
            readBytes: (path, options = {}) => query("read_bytes", { ...options, path }),
            scanText: (path, contains, options = {}) => query("scan_text", { ...options, path, contains }),
            readDocument: path => query("document", { path }),
            // Track file switches without rerunning the rule for every edit.
            documentPath: () => query("document_path", {}),
            list: path => query("list", { path })
        }),
        take() {
            const result = JSON.stringify(queued);
            queued = [];
            return result;
        },
        reply(id, json) {
            const waiter = waiting.get(id);
            if (!waiter) return;
            waiting.delete(id);
            const result = JSON.parse(json);
            if ("Ok" in result) waiter.resolve(result.Ok);
            else waiter.reject(new Error(result.Err));
        }
    };
})
