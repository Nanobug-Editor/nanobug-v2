(module
  (memory (export "memory") 1 2)
  (data (i32.const 0) "Plugin P0")

  (func (export "title_len") (result i32) i32.const 9)
  (func (export "background") (result i32) i32.const 0x26364bff)

  ;; The host stages the confirmed UTF-8 bytes at 512. The guest produces
  ;; the full text draw request at 2048, separate from the input region.
  (func (export "draw") (param $length i32) (result i32)
    i32.const 2048
    i32.const 512
    local.get $length
    memory.copy
    local.get $length)

  ;; A deterministic workload for observing fuel exhaustion in the business lane.
  (func (export "work") (param $spin i32) (result i32)
    local.get $spin
    if (loop $busy (br $busy)) end
    i32.const 42)
)
