use num::{CheckedSub, Num};

/// Get a function that reports whether two numbers are within maxdiff
/// of each other (either direction).
pub fn numbers_within<N>(
    maxdiff: N
) -> impl Fn(N, N) -> bool
where N: CheckedSub + Num + Ord
{
    move |a: N, b: N| -> bool {
        if a > b {
            a.checked_sub(&b).expect("should always work, am I wrong?") < maxdiff
        } else {
            b.checked_sub(&a).expect("should always work, am I wrong?") < maxdiff
        }
    }
}