use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr;

use max_wake::fbank::FEATURES;
use max_wake::spotter::{AcousticModel, BATCH_FRAMES, LOGITS};

type Opaque = c_void;

#[repr(C)]
struct QuantizationParams {
    scale: f32,
    zero_point: i32,
}

/// io.sdk's resolver for spotter models; the stock `TfLiteInterpreterCreate` of this build has no
/// op resolver and crashes.
const RESOLVER_TAG: &CStr = c"SPOTTER_ACTIVATION";
const STATUS_OK: c_int = 0;
const TYPE_FLOAT32: c_int = 1;
const TYPE_INT8: c_int = 9;

struct Api {
    model_create_from_file: unsafe extern "C" fn(*const c_char) -> *mut Opaque,
    model_delete: unsafe extern "C" fn(*mut Opaque),
    options_create: unsafe extern "C" fn() -> *mut Opaque,
    options_set_num_threads: unsafe extern "C" fn(*mut Opaque, i32),
    options_delete: unsafe extern "C" fn(*mut Opaque),
    add_custom_ops: unsafe extern "C" fn(*mut Opaque),
    resolver_create: unsafe extern "C" fn(*const c_char) -> *mut Opaque,
    resolver_delete: unsafe extern "C" fn(*mut Opaque),
    interpreter_create:
        unsafe extern "C" fn(*const Opaque, *mut Opaque, *const Opaque) -> *mut Opaque,
    interpreter_delete: unsafe extern "C" fn(*mut Opaque),
    allocate_tensors: unsafe extern "C" fn(*mut Opaque) -> c_int,
    invoke: unsafe extern "C" fn(*mut Opaque) -> c_int,
    input_count: unsafe extern "C" fn(*const Opaque) -> i32,
    input_tensor: unsafe extern "C" fn(*const Opaque, i32) -> *mut Opaque,
    output_count: unsafe extern "C" fn(*const Opaque) -> i32,
    output_tensor: unsafe extern "C" fn(*const Opaque, i32) -> *const Opaque,
    tensor_type: unsafe extern "C" fn(*const Opaque) -> c_int,
    tensor_name: unsafe extern "C" fn(*const Opaque) -> *const c_char,
    tensor_byte_size: unsafe extern "C" fn(*const Opaque) -> usize,
    tensor_quantization: unsafe extern "C" fn(*const Opaque) -> QuantizationParams,
    copy_from_buffer: unsafe extern "C" fn(*mut Opaque, *const c_void, usize) -> c_int,
    copy_to_buffer: unsafe extern "C" fn(*const Opaque, *mut c_void, usize) -> c_int,
}

impl Api {
    #[allow(clippy::missing_transmute_annotations)]
    fn load(library: &str) -> Result<Self, String> {
        let path = CString::new(library).map_err(|e| e.to_string())?;
        // SAFETY: never dlclosed; signatures from tensorflow/lite/core/c/c_api.h and io.sdk's calls.
        unsafe {
            let lib = libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if lib.is_null() {
                let e = libc::dlerror();
                let why = if e.is_null() {
                    "unknown error".into()
                } else {
                    CStr::from_ptr(e).to_string_lossy()
                };
                return Err(format!("dlopen {library}: {why}"));
            }
            macro_rules! sym {
                ($name:literal) => {{
                    let p = libc::dlsym(lib, concat!($name, "\0").as_ptr().cast());
                    if p.is_null() {
                        return Err(format!("{library}: no {}", $name));
                    }
                    std::mem::transmute::<*mut c_void, _>(p)
                }};
            }
            Ok(Self {
                model_create_from_file: sym!("TfLiteModelCreateFromFile"),
                model_delete: sym!("TfLiteModelDelete"),
                options_create: sym!("TfLiteInterpreterOptionsCreate"),
                options_set_num_threads: sym!("TfLiteInterpreterOptionsSetNumThreads"),
                options_delete: sym!("TfLiteInterpreterOptionsDelete"),
                add_custom_ops: sym!("TfLiteAddCustomOpsRegistrators"),
                resolver_create: sym!("TfLiteCustomOpResolverCreate"),
                resolver_delete: sym!("TfLiteCustomOpResolverDelete"),
                interpreter_create: sym!("TfLiteInterpreterCreateWithCustomResolver"),
                interpreter_delete: sym!("TfLiteInterpreterDelete"),
                allocate_tensors: sym!("TfLiteInterpreterAllocateTensors"),
                invoke: sym!("TfLiteInterpreterInvoke"),
                input_count: sym!("TfLiteInterpreterGetInputTensorCount"),
                input_tensor: sym!("TfLiteInterpreterGetInputTensor"),
                output_count: sym!("TfLiteInterpreterGetOutputTensorCount"),
                output_tensor: sym!("TfLiteInterpreterGetOutputTensor"),
                tensor_type: sym!("TfLiteTensorType"),
                tensor_name: sym!("TfLiteTensorName"),
                tensor_byte_size: sym!("TfLiteTensorByteSize"),
                tensor_quantization: sym!("TfLiteTensorQuantizationParams"),
                copy_from_buffer: sym!("TfLiteTensorCopyFromBuffer"),
                copy_to_buffer: sym!("TfLiteTensorCopyToBuffer"),
            })
        }
    }
}

struct Tensor {
    ptr: *mut Opaque,
    name: String,
    kind: c_int,
    bytes: usize,
    zero_point: i32,
}

struct State {
    input: *mut Opaque,
    output: *const Opaque,
    zero_point: i8,
    scratch: Vec<u8>,
}

pub struct Model {
    api: Api,
    model: *mut Opaque,
    resolver: *mut Opaque,
    interpreter: *mut Opaque,
    frames: *mut Opaque,
    logits: *const Opaque,
    states: Vec<State>,
    lookahead: usize,
}

// SAFETY: owned objects, used by one thread at a time; TFLite keeps no thread-local state.
unsafe impl Send for Model {}

impl Model {
    pub fn load(library: &str, model: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(model).map_err(|e| format!("{}: {e}", model.display()))?;
        let lookahead = max_wake::model_meta::right_context(&bytes)?;
        let api = Api::load(library)?;
        let path = CString::new(model.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
        let mut m = Self {
            api,
            model: ptr::null_mut(),
            resolver: ptr::null_mut(),
            interpreter: ptr::null_mut(),
            frames: ptr::null_mut(),
            logits: ptr::null(),
            states: Vec::new(),
            lookahead,
        };
        // SAFETY: each object is checked before use; Drop frees whatever was created.
        unsafe {
            m.model = (m.api.model_create_from_file)(path.as_ptr());
            if m.model.is_null() {
                return Err(format!("{}: cannot load", model.display()));
            }
            m.resolver = (m.api.resolver_create)(RESOLVER_TAG.as_ptr());
            if m.resolver.is_null() {
                return Err(format!("no TFLite resolver {RESOLVER_TAG:?}"));
            }
            let options = (m.api.options_create)();
            if options.is_null() {
                return Err("TfLiteInterpreterOptionsCreate failed".into());
            }
            (m.api.options_set_num_threads)(options, 1);
            (m.api.add_custom_ops)(options);
            m.interpreter = (m.api.interpreter_create)(m.model, m.resolver, options);
            (m.api.options_delete)(options);
            if m.interpreter.is_null() {
                return Err("TfLiteInterpreterCreate failed".into());
            }
            if (m.api.allocate_tensors)(m.interpreter) != STATUS_OK {
                return Err("TfLiteInterpreterAllocateTensors failed".into());
            }
        }
        m.bind()?;
        m.reset_states();
        Ok(m)
    }

    fn tensors(&self) -> (Vec<Tensor>, Vec<Tensor>) {
        let api = &self.api;
        // SAFETY: indices below the live interpreter's counts; names are NULL or model-owned.
        unsafe {
            let describe = |ptr: *mut Opaque| {
                let name = (api.tensor_name)(ptr);
                Tensor {
                    ptr,
                    name: if name.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(name).to_string_lossy().into_owned()
                    },
                    kind: (api.tensor_type)(ptr),
                    bytes: (api.tensor_byte_size)(ptr),
                    zero_point: (api.tensor_quantization)(ptr).zero_point,
                }
            };
            let inputs = (0..(api.input_count)(self.interpreter))
                .map(|i| describe((api.input_tensor)(self.interpreter, i)))
                .collect();
            let outputs = (0..(api.output_count)(self.interpreter))
                .map(|i| describe((api.output_tensor)(self.interpreter, i).cast_mut()))
                .collect();
            (inputs, outputs)
        }
    }

    fn bind(&mut self) -> Result<(), String> {
        let (inputs, outputs) = self.tensors();
        let float_bytes = size_of::<f32>();
        let l = outputs
            .iter()
            .find(|t| t.name == "logits")
            .ok_or("model has no logits output")?;
        if l.kind != TYPE_FLOAT32 || l.bytes != BATCH_FRAMES * LOGITS * float_bytes {
            return Err(format!("logits: type {} size {}", l.kind, l.bytes));
        }
        self.logits = l.ptr.cast_const();
        for t in inputs {
            if t.name == "frames" {
                if t.kind != TYPE_FLOAT32 || t.bytes != BATCH_FRAMES * FEATURES * float_bytes {
                    return Err(format!("frames: type {} size {}", t.kind, t.bytes));
                }
                self.frames = t.ptr;
                continue;
            }
            let new_name = format!("{}_new", t.name);
            let o = outputs
                .iter()
                .find(|o| o.name == new_name)
                .ok_or_else(|| format!("input {} has no {new_name} output", t.name))?;
            if t.kind != TYPE_INT8 || o.kind != TYPE_INT8 || t.bytes != o.bytes {
                return Err(format!("state {}: not a matching int8 pair", t.name));
            }
            let zero_point = i8::try_from(t.zero_point)
                .map_err(|_| format!("state {}: zero point {}", t.name, t.zero_point))?;
            self.states.push(State {
                input: t.ptr,
                output: o.ptr.cast_const(),
                zero_point,
                scratch: vec![0; t.bytes],
            });
        }
        if self.frames.is_null() {
            return Err("model has no frames input".into());
        }
        Ok(())
    }

    /// io.sdk starts int8 states at their zero point, the quantized 0.0.
    fn reset_states(&mut self) {
        for s in &mut self.states {
            s.scratch.fill(s.zero_point as u8);
        }
        self.load_states();
    }

    fn load_states(&self) {
        for s in &self.states {
            // SAFETY: scratch has the input tensor's byte size (checked in bind).
            unsafe {
                (self.api.copy_from_buffer)(s.input, s.scratch.as_ptr().cast(), s.scratch.len());
            }
        }
    }
}

impl AcousticModel for Model {
    type Error = String;

    fn run(&mut self, features: &[f32], logits: &mut [f32]) -> Result<(), String> {
        if features.len() != BATCH_FRAMES * FEATURES || logits.len() != BATCH_FRAMES * LOGITS {
            return Err("batch size mismatch".into());
        }
        let api = &self.api;
        // SAFETY: every buffer has its tensor's byte size (checked here and in bind).
        unsafe {
            if (api.copy_from_buffer)(self.frames, features.as_ptr().cast(), size_of_val(features))
                != STATUS_OK
            {
                return Err("frames: copy failed".into());
            }
            if (api.invoke)(self.interpreter) != STATUS_OK {
                return Err("TfLiteInterpreterInvoke failed".into());
            }
            if (api.copy_to_buffer)(self.logits, logits.as_mut_ptr().cast(), size_of_val(logits))
                != STATUS_OK
            {
                return Err("logits: copy failed".into());
            }
            for s in &mut self.states {
                (api.copy_to_buffer)(s.output, s.scratch.as_mut_ptr().cast(), s.scratch.len());
            }
        }
        self.load_states();
        Ok(())
    }

    fn reset(&mut self) {
        self.reset_states();
    }

    fn lookahead_frames(&self) -> usize {
        self.lookahead
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        // SAFETY: each pointer is null or a live object from load, freed once, interpreter first.
        unsafe {
            if !self.interpreter.is_null() {
                (self.api.interpreter_delete)(self.interpreter);
            }
            if !self.resolver.is_null() {
                (self.api.resolver_delete)(self.resolver);
            }
            if !self.model.is_null() {
                (self.api.model_delete)(self.model);
            }
        }
    }
}
