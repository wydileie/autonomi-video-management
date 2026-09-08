use crate::{ChunkInfo, DataMap, EncryptedChunk, XorName};
use bytes::Bytes;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyInt, PyTuple};
use std::borrow::Cow;
use std::path::Path;

/// A Python wrapper for the XorName struct.
///
/// XorName is a 32-byte array used for content addressing and chunk identification.
/// It is used to uniquely identify chunks based on their content.
///
/// Example:
///     # Create a XorName from bytes
///     name = PyXorName.from_content(b"some data")
///
///     # Get the underlying bytes
///     bytes = name.as_bytes()
#[pyclass]
#[derive(Clone)]
pub struct PyXorName {
    inner: XorName,
}

#[pymethods]
impl PyXorName {
    /// Create a new XorName from content bytes.
    ///
    /// Args:
    ///     content (bytes): The content to hash into a XorName.
    ///
    /// Returns:
    ///     PyXorName: A new XorName instance.
    #[staticmethod]
    pub fn from_content(content: &[u8]) -> Self {
        Self {
            inner: crate::hash::content_hash(content),
        }
    }

    /// Get the underlying bytes of the XorName.
    ///
    /// Returns:
    ///     bytes: The 32-byte array.
    pub fn as_bytes(&self) -> Cow<'_, [u8]> {
        self.inner.0.to_vec().into()
    }
}

/// A Python wrapper for the ChunkInfo struct.
///
/// ChunkInfo contains metadata about a single chunk in a DataMap,
/// including its index, size, and hashes.
///
/// Example:
///     # Create a ChunkInfo
///     info = PyChunkInfo(index=0, dst_hash=dst, src_hash=src, src_size=1024)
#[pyclass]
#[derive(Clone)]
pub struct PyChunkInfo {
    inner: ChunkInfo,
}

#[pymethods]
impl PyChunkInfo {
    #[new]
    pub fn new(index: usize, dst_hash: PyXorName, src_hash: PyXorName, src_size: usize) -> Self {
        Self {
            inner: ChunkInfo {
                index,
                dst_hash: dst_hash.inner,
                src_hash: src_hash.inner,
                src_size,
            },
        }
    }

    #[getter]
    pub fn index(&self) -> usize {
        self.inner.index
    }

    #[getter]
    pub fn dst_hash(&self) -> PyXorName {
        PyXorName {
            inner: self.inner.dst_hash,
        }
    }

    #[getter]
    pub fn src_hash(&self) -> PyXorName {
        PyXorName {
            inner: self.inner.src_hash,
        }
    }

    #[getter]
    pub fn src_size(&self) -> usize {
        self.inner.src_size
    }
}

/// A Python wrapper for the DataMap struct.
///
/// DataMap contains metadata about encrypted chunks, including their order,
/// sizes, and interdependencies. It is required for decryption.
///
/// Example:
///     # Create a DataMap from JSON
///     data_map = PyDataMap.from_json(json_str)
///
///     # Convert DataMap to JSON for storage
///     json_str = data_map.to_json()
#[pyclass]
#[derive(Clone)]
pub struct PyDataMap {
    inner: DataMap,
}

#[pymethods]
impl PyDataMap {
    /// Create a new DataMap from a JSON string.
    ///
    /// Args:
    ///     json_str (str): A JSON string containing the DataMap data.
    ///
    /// Returns:
    ///     PyDataMap: A new DataMap instance.
    ///
    /// Raises:
    ///     ValueError: If the JSON string is invalid or missing required fields.
    #[staticmethod]
    pub fn from_json(json_str: &str) -> PyResult<Self> {
        let inner = serde_json::from_str(json_str)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Invalid JSON: {e}")))?;
        Ok(Self { inner })
    }

    /// Convert the DataMap to a JSON string.
    ///
    /// Returns:
    ///     str: A JSON string representation of the DataMap.
    pub fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&self.inner).map_err(|e| {
            pyo3::exceptions::PyValueError::new_err(format!("Failed to serialize: {e}"))
        })
    }

    /// Create a new DataMap from a list of chunk infos.
    ///
    /// Args:
    ///     chunk_infos (list[PyChunkInfo]): List of chunk metadata.
    ///
    /// Returns:
    ///     PyDataMap: A new DataMap instance.
    #[new]
    pub fn new(chunk_infos: Vec<PyChunkInfo>) -> Self {
        let inner_infos = chunk_infos.into_iter().map(|info| info.inner).collect();
        Self {
            inner: DataMap::new(inner_infos),
        }
    }

    /// Create a new DataMap with a child level.
    ///
    /// Args:
    ///     chunk_infos (list[PyChunkInfo]): List of chunk metadata.
    ///     child (int): The child level value.
    ///
    /// Returns:
    ///     PyDataMap: A new DataMap instance with the specified child level.
    #[staticmethod]
    pub fn with_child(chunk_infos: Vec<PyChunkInfo>, child: usize) -> Self {
        let inner_infos = chunk_infos.into_iter().map(|info| info.inner).collect();
        Self {
            inner: DataMap::with_child(inner_infos, child),
        }
    }

    /// Get the child level of the DataMap.
    ///
    /// Returns:
    ///     Optional[int]: The child level if present, None otherwise.
    pub fn child(&self) -> Option<usize> {
        self.inner.child()
    }

    /// Get the list of chunk infos.
    ///
    /// Returns:
    ///     list[PyChunkInfo]: The list of chunk metadata.
    pub fn infos(&self) -> Vec<PyChunkInfo> {
        self.inner
            .infos()
            .iter()
            .map(|info| PyChunkInfo {
                inner: info.clone(),
            })
            .collect()
    }

    /// Get the number of chunks in the DataMap.
    ///
    /// Returns:
    ///     int: The number of chunks.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Check if this is a child DataMap.
    ///
    /// Returns:
    ///     bool: True if this is a child DataMap, False otherwise.
    pub fn is_child(&self) -> bool {
        self.inner.is_child()
    }
}

/// A Python wrapper for the EncryptedChunk struct.
///
/// EncryptedChunk represents an encrypted piece of data along with its
/// metadata like size and hash. Chunks are stored separately and
/// referenced by the DataMap.
///
/// Example:
///     # Access chunk metadata
///     chunk = PyEncryptedChunk(...)
///     size = chunk.content_size()
///     hash = chunk.hash()
#[pyclass]
#[derive(Clone)]
pub struct PyEncryptedChunk {
    inner: EncryptedChunk,
}

#[pymethods]
impl PyEncryptedChunk {
    /// Get the size of the original content before encryption.
    ///
    /// Returns:
    ///     int: The size in bytes of the original content.
    pub fn content_size(&self) -> usize {
        self.inner.content.len()
    }

    /// Get the hash of the encrypted chunk.
    ///
    /// Returns:
    ///     bytes: The BLAKE3 hash of the encrypted chunk.
    pub fn hash(&self) -> Cow<'_, [u8]> {
        crate::hash::content_hash(&self.inner.content)
            .0
            .to_vec()
            .into()
    }
}

/// Encrypt raw data into chunks.
///
/// This function takes raw data, splits it into chunks, encrypts them,
/// and returns a DataMap and list of encrypted chunks.
///
/// Args:
///     data (bytes): The data to encrypt.
///
/// Returns:
///     tuple: A tuple containing:
///         - PyDataMap: The data map required for decryption
///         - list[PyEncryptedChunk]: The list of encrypted chunks
///
/// Raises:
///     ValueError: If the data is too small (less than MIN_ENCRYPTABLE_BYTES).
#[pyfunction]
pub fn encrypt(data: &[u8]) -> PyResult<(PyDataMap, Vec<PyEncryptedChunk>)> {
    let bytes = Bytes::copy_from_slice(data);
    let (data_map, chunks) = crate::encrypt(bytes)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Encryption failed: {e}")))?;
    let py_chunks = chunks
        .into_iter()
        .map(|chunk| PyEncryptedChunk { inner: chunk })
        .collect();
    Ok((PyDataMap { inner: data_map }, py_chunks))
}

/// Decrypt data using a DataMap and chunks.
///
/// This function takes a DataMap and list of encrypted chunks,
/// decrypts them, and returns the original data.
///
/// Args:
///     data_map (PyDataMap): The data map containing chunk metadata.
///     chunks (list[PyEncryptedChunk]): The list of encrypted chunks.
///
/// Returns:
///     bytes: The decrypted data.
///
/// Raises:
///     ValueError: If decryption fails or chunks are missing/corrupted.
#[pyfunction]
pub fn decrypt(
    data_map: &PyDataMap,
    chunks: Vec<PyEncryptedChunk>,
) -> PyResult<std::borrow::Cow<'_, [u8]>> {
    let inner_chunks = chunks
        .into_iter()
        .map(|chunk| chunk.inner)
        .collect::<Vec<_>>();
    let bytes = crate::decrypt(&data_map.inner, &inner_chunks)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Decryption failed: {e}")))?;
    Ok(bytes.to_vec().into())
}

/// Encrypt a file and store its chunks.
///
/// This function reads a file, splits it into chunks, encrypts them,
/// and stores them in the specified directory.
///
/// Args:
///     input_file (str): Path to the file to encrypt.
///     output_dir (str): Directory to store the encrypted chunks.
///
/// Returns:
///     tuple: A tuple containing:
///         - PyDataMap: The data map required for decryption
///         - list: List of chunk filenames that were created
///
/// Raises:
///     OSError: If the input file cannot be read or chunks cannot be written.
///     ValueError: If the input parameters are invalid.
#[pyfunction]
pub fn encrypt_from_file(input_file: &str, output_dir: &str) -> PyResult<(PyDataMap, Vec<String>)> {
    use std::io::{Read, Write};

    let input_path = Path::new(input_file);
    let output_path = Path::new(output_dir);

    let file = std::fs::File::open(input_path).map_err(|e| {
        pyo3::exceptions::PyOSError::new_err(format!("Failed to open input file: {e}"))
    })?;
    let data_size = file
        .metadata()
        .map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("Failed to get file metadata: {e}"))
        })?
        .len() as usize;
    let mut reader = std::io::BufReader::new(file);

    // Track read errors so they aren't silently swallowed by the iterator
    let read_error: std::rc::Rc<std::cell::RefCell<Option<std::io::Error>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let read_error_clone = read_error.clone();

    let data_iter = std::iter::from_fn(move || {
        let mut buffer = vec![0u8; 8192];
        match reader.read(&mut buffer) {
            Ok(0) => None,
            Ok(n) => {
                buffer.truncate(n);
                Some(Bytes::from(buffer))
            }
            Err(e) => {
                *read_error_clone.borrow_mut() = Some(e);
                None
            }
        }
    });

    let mut stream = crate::stream_encrypt(data_size, data_iter)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("Encryption failed: {e}")))?;

    let mut chunk_names = Vec::new();
    for chunk_result in stream.chunks() {
        let (hash, content) = chunk_result
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("Encryption error: {e}")))?;
        chunk_names.push(hex::encode(hash.0));

        let chunk_path = output_path.join(hex::encode(hash));
        let mut output_file = std::fs::File::create(&chunk_path).map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("Failed to create chunk file: {e}"))
        })?;
        output_file.write_all(&content).map_err(|e| {
            pyo3::exceptions::PyOSError::new_err(format!("Failed to write chunk: {e}"))
        })?;
    }

    if let Some(e) = read_error.borrow_mut().take() {
        return Err(pyo3::exceptions::PyOSError::new_err(format!(
            "Failed to read input file: {e}"
        )));
    }

    let data_map = stream.into_datamap().ok_or_else(|| {
        pyo3::exceptions::PyValueError::new_err(
            "Encryption did not produce a DataMap — ensure chunks() iterator was fully consumed",
        )
    })?;

    Ok((PyDataMap { inner: data_map }, chunk_names))
}

/// Decrypt data using a DataMap and stored chunks (single-chunk callback).
///
/// This function retrieves encrypted chunks one at a time using the provided
/// callback, decrypts them according to the DataMap, and writes the result
/// to a file.
///
/// Args:
///     data_map (PyDataMap): The data map containing chunk metadata.
///     output_file (str): Path where the decrypted data will be written.
///     get_chunk (callable): A function with signature `get_chunk(name: str) -> bytes`
///         that retrieves a single encrypted chunk by its hex-encoded XorName.
///         Returns the raw chunk bytes.
///
/// Raises:
///     OSError: If chunks cannot be retrieved or output cannot be written.
///     ValueError: If the data map is invalid or chunks are corrupted.
#[pyfunction]
pub fn decrypt_from_storage(
    data_map: PyDataMap,
    output_file: &str,
    get_chunk: Bound<'_, PyAny>,
) -> PyResult<()> {
    let output_path = Path::new(output_file);

    // Wrap the single-chunk callback into a parallel batch callback for streaming_decrypt
    let get_chunk_parallel = |hashes: &[(usize, XorName)]| -> crate::Result<Vec<(usize, Bytes)>> {
        hashes
            .iter()
            .map(|(i, hash)| {
                let name_str = hex::encode(hash.0);
                let chunk = get_chunk
                    .call1((name_str,))
                    .map_err(|e| crate::Error::Python(format!("Failed to call get_chunk: {e}")))?;
                let bytes = chunk.downcast::<PyBytes>().map_err(|e| {
                    crate::Error::Python(format!("get_chunk must return bytes: {e}"))
                })?;
                Ok((*i, Bytes::copy_from_slice(bytes.as_bytes())))
            })
            .collect()
    };

    let stream = crate::streaming_decrypt(&data_map.inner, &get_chunk_parallel)
        .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("Decryption failed: {e}")))?;

    let mut output = std::fs::File::create(output_path).map_err(|e| {
        pyo3::exceptions::PyOSError::new_err(format!("Failed to create output: {e}"))
    })?;

    for chunk_result in stream {
        let chunk = chunk_result
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("Decryption failed: {e}")))?;
        std::io::Write::write_all(&mut output, &chunk)
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("Write failed: {e}")))?;
    }

    Ok(())
}

/// Decrypt data using streaming for better performance with large files (batch callback).
///
/// This function uses batch chunk retrieval and streaming to efficiently
/// decrypt large files while minimizing memory usage.
///
/// Args:
///     data_map (PyDataMap): The data map containing chunk metadata.
///     output_file (str): Path where the decrypted data will be written.
///     get_chunks (callable): A function with signature
///         `get_chunks(names: list[tuple[int, str]]) -> list[tuple[int, bytes]]`
///         that retrieves multiple encrypted chunks in one call.
///         Input: list of `(chunk_index, hex_encoded_xorname)` tuples.
///         Output: list of `(chunk_index, raw_chunk_bytes)` tuples.
///         Example:
///             get_chunks([(0, "abc123..."), (1, "def456...")])
///             -> [(0, b"chunk0_data"), (1, b"chunk1_data")]
///
/// Raises:
///     OSError: If chunks cannot be retrieved or output cannot be written.
///     ValueError: If the data map is invalid or chunks are corrupted.
#[pyfunction]
pub fn streaming_decrypt_from_storage(
    data_map: PyDataMap,
    output_file: &str,
    get_chunks: Bound<'_, PyAny>,
) -> PyResult<()> {
    use std::io::Write;

    let output_path = Path::new(output_file);
    let get_chunks_wrapper = |names: &[(usize, XorName)]| -> crate::Result<Vec<(usize, Bytes)>> {
        let name_strs: Vec<(usize, String)> =
            names.iter().map(|(i, x)| (*i, hex::encode(x.0))).collect();
        let chunks = get_chunks
            .call1((name_strs,))
            .map_err(|e| crate::Error::Python(format!("Failed to call get_chunks: {e}")))?;
        let chunks = chunks
            .try_iter()
            .map_err(|e| crate::Error::Python(format!("get_chunks must return a list: {e}")))?;
        let mut result = Vec::new();
        for chunk in chunks {
            let chunk = chunk
                .map_err(|e| crate::Error::Python(format!("Failed to iterate chunks: {e}")))?;

            let chunk_tuple = chunk
                .downcast::<PyTuple>()
                .map_err(|e| crate::Error::Python(format!("get_chunks must return tuple: {e}")))?;

            if chunk_tuple.len() != 2 {
                return Err(crate::Error::Python(
                    "get_chunks must return tuples of length 2".to_string(),
                ));
            }

            let index_item = chunk_tuple.get_item(0)?;
            let index = index_item
                .downcast::<PyInt>()
                .map_err(|e| crate::Error::Python(format!("First element must be integer: {e}")))?
                .extract::<usize>()
                .map_err(|e| crate::Error::Python(format!("Failed to extract index: {e}")))?;

            let bytes_item = chunk_tuple.get_item(1)?;
            let bytes = bytes_item
                .downcast::<PyBytes>()
                .map_err(|e| crate::Error::Python(format!("Second element must be bytes: {e}")))?;

            result.push((index, Bytes::copy_from_slice(bytes.as_bytes())));
        }
        Ok(result)
    };

    // Use streaming_decrypt internally, writing output to file
    let stream = crate::streaming_decrypt(&data_map.inner, get_chunks_wrapper).map_err(|e| {
        pyo3::exceptions::PyOSError::new_err(format!("Streaming decryption failed: {e}"))
    })?;

    let mut file = std::fs::File::create(output_path).map_err(|e| {
        pyo3::exceptions::PyOSError::new_err(format!("Failed to create output file: {e}"))
    })?;

    for chunk_result in stream {
        let chunk = chunk_result
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("Decryption error: {e}")))?;
        file.write_all(&chunk)
            .map_err(|e| pyo3::exceptions::PyOSError::new_err(format!("Write error: {e}")))?;
    }

    Ok(())
}

/// Initialize the Python module.
#[pymodule]
#[pyo3(name = "_self_encryption")]
fn self_encryption_module(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyDataMap>()?;
    m.add_class::<PyEncryptedChunk>()?;
    m.add_class::<PyXorName>()?;
    m.add_class::<PyChunkInfo>()?;
    m.add_function(wrap_pyfunction!(encrypt, m)?)?;
    m.add_function(wrap_pyfunction!(decrypt, m)?)?;
    m.add_function(wrap_pyfunction!(encrypt_from_file, m)?)?;
    m.add_function(wrap_pyfunction!(decrypt_from_storage, m)?)?;
    m.add_function(wrap_pyfunction!(streaming_decrypt_from_storage, m)?)?;
    Ok(())
}
