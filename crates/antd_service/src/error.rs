// The service-specific ApiError was unified into autvid_common; the error
// body is now `{"detail": ..., "code": ...}` (previously `{"error", "code"}`).
pub(crate) use autvid_common::ApiError;
