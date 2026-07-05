#[cxx::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("beater-agent/src/cpp_tools.h");

        fn cpp_double(input: i64) -> i64;
    }
}

pub fn double(input: i64) -> i64 {
    ffi::cpp_double(input)
}
