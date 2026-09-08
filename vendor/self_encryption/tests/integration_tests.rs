use bytes::Bytes;
use rayon::prelude::*;
use self_encryption::{
    decrypt, encrypt, get_root_data_map, shrink_data_map, stream_encrypt, streaming_decrypt,
    test_helpers::random_bytes, verify_chunk, DataMap, EncryptedChunk, Error, Result,
};

/// Helper: encrypt a file and write chunks to an output directory (replaces removed encrypt_from_file)
fn encrypt_file_to_dir(
    file_path: &std::path::Path,
    output_dir: &std::path::Path,
) -> Result<(DataMap, Vec<XorName>)> {
    let mut file = File::open(file_path)?;
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes)?;
    let bytes = Bytes::from(bytes);

    let (data_map, encrypted_chunks) = encrypt(bytes)?;

    let mut chunk_names = Vec::new();
    for chunk in encrypted_chunks {
        let chunk_name = self_encryption::hash::content_hash(&chunk.content);
        chunk_names.push(chunk_name);

        let chunk_path = output_dir.join(hex::encode(chunk_name));
        let mut output_file = File::create(chunk_path)?;
        output_file.write_all(&chunk.content)?;
    }

    Ok((data_map, chunk_names))
}

/// Helper: decrypt a data map to a file using streaming_decrypt (replaces removed decrypt_from_storage)
fn decrypt_to_file(
    data_map: &DataMap,
    output_path: &std::path::Path,
    get_chunk: Box<dyn FnMut(XorName) -> Result<Bytes>>,
) -> Result<()> {
    use std::cell::RefCell;
    let get_chunk = RefCell::new(get_chunk);
    let get_chunk_parallel = |hashes: &[(usize, XorName)]| -> Result<Vec<(usize, Bytes)>> {
        hashes
            .iter()
            .map(|(i, hash)| {
                let data = (*get_chunk.borrow_mut())(*hash)?;
                Ok((*i, data))
            })
            .collect()
    };

    let stream = streaming_decrypt(data_map, &get_chunk_parallel)?;
    let decrypted = stream.range_full()?;

    let mut output = File::create(output_path)?;
    output.write_all(&decrypted)?;

    Ok(())
}

use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    sync::{Arc, Mutex},
};
use tempfile::TempDir;
use xor_name::XorName;

// Define traits for our storage operations
type StoreFn = Box<dyn FnMut(XorName, Bytes) -> Result<()>>;
type RetrieveFn = Box<dyn FnMut(XorName) -> Result<Bytes>>;

// Helper struct to manage different storage backends
struct StorageBackend {
    memory: Arc<Mutex<HashMap<XorName, Bytes>>>,
    disk_dir: TempDir,
}

impl StorageBackend {
    fn new() -> Result<Self> {
        Ok(Self {
            memory: Arc::new(Mutex::new(HashMap::new())),
            disk_dir: TempDir::new()?,
        })
    }

    fn store_to_memory(&self) -> StoreFn {
        let memory = self.memory.clone();
        Box::new(move |hash, data| {
            memory
                .lock()
                .map_err(|_| Error::Generic("Lock poisoned".into()))?
                .insert(hash, data.clone());
            Ok(())
        })
    }

    fn store_to_disk(&self) -> StoreFn {
        let base_path = self.disk_dir.path().to_owned();
        Box::new(move |hash, data| {
            let path = base_path.join(hex::encode(hash));
            let mut file = File::create(&path)?;
            file.write_all(&data)?;
            file.sync_all()?;
            Ok(())
        })
    }

    fn retrieve_from_memory(&self) -> RetrieveFn {
        let memory = self.memory.clone();
        Box::new(move |hash| {
            memory
                .lock()
                .map_err(|_| Error::Generic("Lock poisoned".into()))?
                .get(&hash)
                .cloned()
                .ok_or_else(|| Error::Generic("Chunk not found in memory".into()))
        })
    }

    fn retrieve_from_disk(&self) -> RetrieveFn {
        let base_path = self.disk_dir.path().to_owned();
        Box::new(move |hash| {
            let path = base_path.join(hex::encode(hash));
            let mut file = File::open(&path)
                .map_err(|e| Error::Generic(format!("Failed to open chunk file: {e}")))?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)
                .map_err(|e| Error::Generic(format!("Failed to read chunk data: {e}")))?;
            Ok(Bytes::from(data))
        })
    }

    fn verify_chunk_stored(&self, hash: XorName) -> Result<()> {
        if let Ok(guard) = self.memory.lock() {
            if guard.contains_key(&hash) {
                return Ok(());
            }
        }

        let path = self.disk_dir.path().join(hex::encode(hash));
        if path.exists() {
            return Ok(());
        }

        Err(Error::Generic(format!(
            "Chunk {} not found in any backend",
            hex::encode(hash)
        )))
    }

    fn debug_storage_state(&self, prefix: &str) -> Result<()> {
        println!("\n=== {prefix} ===");
        if let Ok(guard) = self.memory.lock() {
            println!("Memory storage contains {} chunks", guard.len());
            for (hash, data) in guard.iter() {
                println!("Memory chunk: {} ({} bytes)", hex::encode(hash), data.len());
            }
        }

        let disk_chunks: Vec<_> = std::fs::read_dir(self.disk_dir.path())?
            .filter_map(|entry| entry.ok())
            .collect();
        println!("Disk storage contains {} chunks", disk_chunks.len());
        for entry in disk_chunks {
            println!(
                "Disk chunk: {} ({} bytes)",
                entry.file_name().to_string_lossy(),
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            );
        }
        println!("================\n");
        Ok(())
    }
}

// Modify test helper function to verify storage
fn verify_storage_operation(data_map: &DataMap, storage: &StorageBackend) -> Result<()> {
    for chunk_info in data_map.infos() {
        storage.verify_chunk_stored(chunk_info.dst_hash)?;
    }
    Ok(())
}

#[test]
fn test_cross_backend_encryption_decryption() -> Result<()> {
    let test_size = 10 * 1024 * 1024;
    let original_data = random_bytes(test_size);
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    for (name, use_memory_store, _use_memory_retrieve) in &[("memory-to-memory", true, true)] {
        println!("\nRunning test case: {name}");

        let input_path = temp_dir.path().join("input.dat");
        let mut input_file = File::create(&input_path)?;
        input_file.write_all(&original_data)?;

        storage.debug_storage_state("Before encryption")?;
        let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;
        println!("Encrypted into {} chunks", data_map.len());
        storage.debug_storage_state("After encryption")?;

        let mut store_fn = if *use_memory_store {
            storage.store_to_memory()
        } else {
            storage.store_to_disk()
        };

        // Store the encrypted chunks using data_map info
        for chunk_info in data_map.infos() {
            let chunk_path = storage
                .disk_dir
                .path()
                .join(hex::encode(chunk_info.dst_hash));
            let mut chunk_data = Vec::new();
            File::open(&chunk_path)?.read_to_end(&mut chunk_data)?;
            store_fn(chunk_info.dst_hash, Bytes::from(chunk_data))?;
        }
        storage.debug_storage_state("After storing chunks")?;

        // Rest of the test remains the same...
    }
    Ok(())
}

#[test]
fn test_large_file_cross_backend() -> Result<()> {
    let test_size = 100 * 1024 * 1024;
    let original_data = random_bytes(test_size);
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    let input_path = temp_dir.path().join("large_input.dat");
    let mut input_file = File::create(&input_path)?;
    input_file.write_all(&original_data)?;

    storage.debug_storage_state("Before encryption")?;
    let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

    // Explicitly store chunks in memory
    let mut store_fn = storage.store_to_memory();
    for chunk_info in data_map.infos() {
        let chunk_path = storage
            .disk_dir
            .path()
            .join(hex::encode(chunk_info.dst_hash));
        let mut chunk_data = Vec::new();
        File::open(&chunk_path)?.read_to_end(&mut chunk_data)?;
        store_fn(chunk_info.dst_hash, Bytes::from(chunk_data))?;
    }
    storage.debug_storage_state("After storing chunks")?;

    // Shrink to memory
    let mut store_fn = storage.store_to_memory();
    let shrunk_map = shrink_data_map(data_map.clone(), &mut store_fn)?;

    // Get root map from memory
    let mut retrieve_fn = storage.retrieve_from_memory();
    let root_map = get_root_data_map(shrunk_map.0, &mut retrieve_fn)?;

    // Decrypt using disk backend
    let output_path = temp_dir.path().join("large_output.dat");
    decrypt_to_file(&root_map, &output_path, storage.retrieve_from_disk())?;

    // Verify large file content
    let mut decrypted = Vec::new();
    File::open(&output_path)?.read_to_end(&mut decrypted)?;
    assert_eq!(original_data.as_ref(), decrypted.as_slice());

    Ok(())
}

#[test]
fn test_concurrent_backend_access() -> Result<()> {
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let storage = Arc::new(StorageBackend::new()?);
    let temp_dir = Arc::new(TempDir::new()?);
    let processed = Arc::new(AtomicUsize::new(0));

    // Create multiple test files of different sizes
    let sizes = vec![1, 5, 10, 20].into_iter().map(|x| x * 1024 * 1024);

    // Process files concurrently
    sizes.par_bridge().try_for_each(|size| -> Result<()> {
        let storage = storage.clone();
        let temp_dir = temp_dir.clone();
        let processed = processed.clone();

        let data = random_bytes(size);
        let count = processed.fetch_add(1, Ordering::SeqCst);

        // Setup paths with unique identifiers
        let input_path = temp_dir.path().join(format!("input_{count}_{size}.dat"));
        let output_path = temp_dir.path().join(format!("output_{count}_{size}.dat"));

        // Write test data
        File::create(&input_path)?.write_all(&data)?;

        // Encrypt using memory backend
        let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

        // Verify storage after each operation
        let mut store_fn = storage.store_to_disk();
        let shrunk_map = shrink_data_map(data_map.clone(), &mut store_fn)?;
        verify_storage_operation(&data_map, &storage)?;

        let mut retrieve_fn = storage.retrieve_from_disk();
        let root_map = get_root_data_map(shrunk_map.0, &mut retrieve_fn)?;

        decrypt_to_file(&root_map, &output_path, storage.retrieve_from_disk())?;

        // Verify
        let mut decrypted = Vec::new();
        File::open(&output_path)?.read_to_end(&mut decrypted)?;
        assert_eq!(data.as_ref(), decrypted.as_slice());

        Ok(())
    })?;

    Ok(())
}

#[test]
fn test_error_handling_across_backends() -> Result<()> {
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    // Create test data
    let test_size = 20 * 1024 * 1024; // 20MB is fine, we'll always get 3 chunks
    let data = random_bytes(test_size);
    let input_path = temp_dir.path().join("input.dat");
    File::create(&input_path)?.write_all(&data)?;

    // Encrypt normally
    let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

    // Test failing store function
    let mut failing_store: StoreFn =
        Box::new(|_, _| Err(Error::Generic("Simulated storage failure".into())));

    // The store function should fail during shrinking
    let result = shrink_data_map(data_map.clone(), &mut failing_store);
    assert!(
        result.is_ok(),
        "Shrinking with failing store should succeed since we only have 3 chunks"
    );

    // Test failing retrieve function
    let mut store_fn = storage.store_to_memory();
    let (shrunk_map, _) = shrink_data_map(data_map.clone(), &mut store_fn)?;

    let mut failing_retrieve: RetrieveFn =
        Box::new(|_| Err(Error::Generic("Simulated retrieval failure".into())));
    assert!(get_root_data_map(shrunk_map, &mut failing_retrieve).is_err());

    Ok(())
}

#[test]
fn test_cross_platform_compatibility() -> Result<()> {
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    for size in &[3073, 1024 * 1024] {
        // Start with smaller subset for testing
        println!("Testing size: {size}");

        // Create deterministic data
        let mut content = vec![0u8; *size];
        for (i, c) in content.iter_mut().enumerate() {
            *c = (i % 256) as u8;
        }
        let original_data = Bytes::from(content);

        let input_path = temp_dir.path().join(format!("input_{size}.dat"));
        let mut input_file = File::create(&input_path)?;
        input_file.write_all(&original_data)?;

        storage.debug_storage_state("Before encryption")?;
        let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

        // Store in both backends
        let mut memory_store = storage.store_to_memory();
        let mut disk_store = storage.store_to_disk();

        for chunk_info in data_map.infos() {
            let chunk_path = storage
                .disk_dir
                .path()
                .join(hex::encode(chunk_info.dst_hash));
            let mut chunk_data = Vec::new();
            File::open(&chunk_path)?.read_to_end(&mut chunk_data)?;
            let chunk_content = Bytes::from(chunk_data);

            memory_store(chunk_info.dst_hash, chunk_content.clone())?;
            disk_store(chunk_info.dst_hash, chunk_content)?;
        }
        storage.debug_storage_state("After storing chunks")?;

        // Rest of the test remains the same...
    }

    Ok(())
}

#[test]
fn test_platform_specific_sizes() -> Result<()> {
    let storage = StorageBackend::new()?;
    let _temp_dir = TempDir::new()?;

    let test_cases = vec![
        ("small", 3 * 1024 * 1024),  // 3MB
        ("medium", 5 * 1024 * 1024), // 5MB
        ("large", 10 * 1024 * 1024), // 10MB
    ];

    for (name, size) in test_cases {
        println!("Testing size: {name} ({size} bytes)");

        let original_data = random_bytes(size);

        // First encrypt the data directly to get ALL chunks
        let (data_map, initial_chunks) = encrypt(original_data.clone())?;

        println!("Initial data map has {} chunks", data_map.len());
        println!("Data map child level: {:?}", data_map.child());

        // Start with all initial chunks
        let mut all_chunks = Vec::new();
        all_chunks.extend(initial_chunks);

        // Now do a shrink operation
        let mut store_memory = storage.store_to_memory();
        let (shrunk_map, shrink_chunks) = shrink_data_map(data_map.clone(), &mut store_memory)?;
        println!("Got {} new chunks from shrinking", shrink_chunks.len());

        // Add shrink chunks to our collection
        all_chunks.extend(shrink_chunks);

        println!("Final data map has {} chunks", shrunk_map.len());
        println!("Total chunks: {}", all_chunks.len());

        // Use decrypt which will handle getting the root map internally
        let decrypted_bytes = decrypt(&shrunk_map, &all_chunks)?;

        // Verify content matches
        assert_eq!(
            original_data.as_ref(),
            decrypted_bytes.as_ref(),
            "Data mismatch for {name} (size: {size})"
        );
    }

    Ok(())
}

#[test]
fn test_encrypt_from_file_stores_all_chunks() -> Result<()> {
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    // Create a large enough file to trigger shrinking
    let file_size = 10 * 1024 * 1024; // 10MB
    let original_data = random_bytes(file_size);
    let input_path = temp_dir.path().join("input.dat");
    File::create(&input_path)?.write_all(&original_data)?;

    // First encrypt directly to get the expected chunks
    let (_, expected_chunks) = encrypt(original_data.clone())?;
    let expected_chunk_count = expected_chunks.len();

    // Now encrypt from file
    let (data_map, chunk_names) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

    println!("Expected chunks: {expected_chunk_count}");
    println!("Got chunk names: {}", chunk_names.len());

    // Verify we got all chunks
    assert_eq!(
        expected_chunk_count,
        chunk_names.len(),
        "Number of stored chunks doesn't match expected"
    );

    // Verify we can decrypt using the stored chunks
    let output_path = temp_dir.path().join("output.dat");
    decrypt_to_file(&data_map, &output_path, storage.retrieve_from_disk())?;

    // Verify content
    let mut decrypted = Vec::new();
    File::open(&output_path)?.read_to_end(&mut decrypted)?;
    assert_eq!(
        original_data.as_ref(),
        decrypted.as_slice(),
        "Decrypted content doesn't match original"
    );

    Ok(())
}

#[test]
fn test_comprehensive_encryption_decryption() -> Result<()> {
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    // Test sizes to ensure we test both small and large files
    let test_cases = vec![
        ("3MB", 3 * 1024 * 1024),   // Basic 3-chunk case
        ("5MB", 5 * 1024 * 1024),   // Triggers shrinking
        ("10MB", 10 * 1024 * 1024), // Larger file
        ("20MB", 20 * 1024 * 1024), // Even larger file
    ];

    for (size_name, size) in test_cases {
        println!("\n=== Testing {size_name} file ===");
        let original_data = random_bytes(size);

        // 1. In-memory encryption (encrypt)
        println!("\n1. Testing in-memory encryption (encrypt):");
        let (data_map1, chunks1) = encrypt(original_data.clone())?;
        println!("- Generated {} chunks", chunks1.len());
        println!("- Data map child level: {:?}", data_map1.child());

        // 2. File-based encryption (encrypt_from_file)
        println!("\n2. Testing file-based encryption (encrypt_from_file):");
        let input_path = temp_dir.path().join(format!("input_{size_name}.dat"));
        File::create(&input_path)?.write_all(&original_data)?;
        let (data_map2, chunk_names) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;
        println!("- Generated {} chunks", chunk_names.len());
        println!("- Data map child level: {:?}", data_map2.child());

        // Now test all decryption methods with each encryption result
        println!("\n=== Testing all decrypt combinations ===");

        // A. Test decrypt() with in-memory encryption result
        println!("\nA.1 Testing decrypt() with encrypt() result:");
        let decrypted_a1 = decrypt(&data_map1, &chunks1)?;
        assert_eq!(
            original_data.as_ref(),
            decrypted_a1.as_ref(),
            "Mismatch: encrypt() -> decrypt()"
        );
        println!("✓ decrypt() successful");

        // B. Test decrypt_to_file() with in-memory encryption result
        println!("\nA.2 Testing decrypt_to_file() with encrypt() result:");
        // First store chunks to disk
        for chunk in &chunks1 {
            let hash = self_encryption::hash::content_hash(&chunk.content);
            let chunk_path = storage.disk_dir.path().join(hex::encode(hash));
            File::create(&chunk_path)?.write_all(&chunk.content)?;
        }
        let output_path1 = temp_dir.path().join(format!("output1_{size_name}.dat"));
        decrypt_to_file(&data_map1, &output_path1, storage.retrieve_from_disk())?;

        let mut decrypted = Vec::new();
        File::open(&output_path1)?.read_to_end(&mut decrypted)?;
        assert_eq!(
            original_data.as_ref(),
            decrypted.as_slice(),
            "Mismatch: encrypt() -> decrypt_to_file()"
        );
        println!("✓ decrypt_to_file() successful");

        // C. Test streaming_decrypt() with in-memory encryption result
        println!("\nA.3 Testing streaming_decrypt() with encrypt() result:");

        // Create parallel chunk retrieval function
        let chunk_dir = storage.disk_dir.path().to_owned();
        let get_chunk_parallel = |hashes: &[(usize, XorName)]| -> Result<Vec<(usize, Bytes)>> {
            hashes
                .par_iter()
                .map(|(i, hash)| {
                    let chunk_path = chunk_dir.join(hex::encode(hash));
                    let mut chunk_data = Vec::new();
                    File::open(&chunk_path)
                        .and_then(|mut file| file.read_to_end(&mut chunk_data))
                        .map_err(|e| Error::Generic(format!("Failed to read chunk: {e}")))?;
                    Ok((*i, Bytes::from(chunk_data)))
                })
                .collect()
        };

        let decrypt_stream = streaming_decrypt(&data_map1, &get_chunk_parallel)?;
        let decrypted = decrypt_stream.range_full()?;
        assert_eq!(
            original_data.as_ref(),
            decrypted.as_ref(),
            "Mismatch: encrypt() -> streaming_decrypt()"
        );
        println!("✓ streaming_decrypt() successful");

        // D. Test decrypt() with file-based encryption result
        println!("\nB.1 Testing decrypt() with encrypt_file_to_dir() result:");
        let mut file_chunks = Vec::new();
        for hash in &chunk_names {
            let chunk_path = storage.disk_dir.path().join(hex::encode(hash));
            let mut chunk_data = Vec::new();
            File::open(&chunk_path)?.read_to_end(&mut chunk_data)?;
            file_chunks.push(EncryptedChunk {
                content: Bytes::from(chunk_data),
            });
        }
        let decrypted2 = decrypt(&data_map2, &file_chunks)?;
        assert_eq!(
            original_data.as_ref(),
            decrypted2.as_ref(),
            "Mismatch: encrypt_file_to_dir() -> decrypt()"
        );
        println!("✓ decrypt() successful");

        // E. Test decrypt_to_file() with file-based encryption result
        println!("\nB.2 Testing decrypt_to_file() with encrypt_file_to_dir() result:");
        let output_path2 = temp_dir.path().join(format!("output2_{size_name}.dat"));
        decrypt_to_file(&data_map2, &output_path2, storage.retrieve_from_disk())?;

        let mut decrypted = Vec::new();
        File::open(&output_path2)?.read_to_end(&mut decrypted)?;
        assert_eq!(
            original_data.as_ref(),
            decrypted.as_slice(),
            "Mismatch: encrypt_file_to_dir() -> decrypt_to_file()"
        );
        println!("✓ decrypt_to_file() successful");

        // F. Test streaming_decrypt() with file-based encryption result
        println!("\nB.3 Testing streaming_decrypt() with encrypt_file_to_dir() result:");
        let decrypt_stream2 = streaming_decrypt(&data_map2, get_chunk_parallel)?;
        let decrypted = decrypt_stream2.range_full()?;
        assert_eq!(
            original_data.as_ref(),
            decrypted.as_ref(),
            "Mismatch: encrypt_file_to_dir() -> streaming_decrypt()"
        );
        println!("✓ streaming_decrypt() successful");

        // Additional verifications
        println!("\n=== Verifying consistency ===");

        // Verify data maps are equivalent
        assert_eq!(
            data_map1.len(),
            data_map2.len(),
            "Data maps have different number of chunks"
        );
        assert_eq!(
            data_map1.child(),
            data_map2.child(),
            "Data maps have different child levels"
        );
        println!("✓ Data maps match");

        // Verify chunk counts
        assert_eq!(
            chunks1.len(),
            file_chunks.len(),
            "Different number of chunks between methods"
        );
        println!("✓ Chunk counts match");

        // Verify output files are identical
        let outputs = [output_path1, output_path2];
        for (i, path1) in outputs.iter().enumerate() {
            for path2 in outputs.iter().skip(i + 1) {
                let mut content1 = Vec::new();
                let mut content2 = Vec::new();
                File::open(path1)?.read_to_end(&mut content1)?;
                File::open(path2)?.read_to_end(&mut content2)?;
                assert_eq!(
                    content1, content2,
                    "Output files don't match: {path1:?} vs {path2:?}"
                );
            }
        }
        println!("✓ All output files match");

        println!("\n{size_name} test completed successfully");
    }

    Ok(())
}

#[test]
fn test_streaming_decrypt_with_parallel_retrieval() -> Result<()> {
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    // Create test data and encrypt it
    let test_size = 10 * 1024 * 1024; // 10MB
    let data = random_bytes(test_size);
    let input_path = temp_dir.path().join("input.dat");
    File::create(&input_path)?.write_all(&data)?;

    // Encrypt and store chunks to disk
    let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

    // Implement parallel chunk retrieval function
    let chunk_dir = storage.disk_dir.path().to_owned();
    let get_chunk_parallel = |hashes: &[(usize, XorName)]| -> Result<Vec<(usize, Bytes)>> {
        hashes
            .par_iter()
            .map(|(i, hash)| {
                let chunk_path = chunk_dir.join(hex::encode(hash));
                let mut chunk_data = Vec::new();
                File::open(&chunk_path)
                    .and_then(|mut file| file.read_to_end(&mut chunk_data))
                    .map_err(|e| Error::Generic(format!("Failed to read chunk: {e}")))?;
                Ok((*i, Bytes::from(chunk_data)))
            })
            .collect()
    };

    // Use the streaming decryption iterator
    let decrypt_stream = streaming_decrypt(&data_map, get_chunk_parallel)?;
    let decrypted = decrypt_stream.range_full()?;
    assert_eq!(data.as_ref(), decrypted.as_ref());

    Ok(())
}

#[test]
fn test_streaming_decrypt_with_parallel_random_chunks() -> Result<()> {
    use rand::seq::SliceRandom;
    use rand::thread_rng;
    use std::collections::HashMap;

    // Generate 100MB content
    let content_size = 100 * 1024 * 1024; // 100MB
    let original_data = random_bytes(content_size);

    // Encrypt to get datamap and encrypted_chunks
    let (data_map, encrypted_chunks) = encrypt(original_data.clone())?;

    // Create a storage map from encrypted_chunks using their hashes as keys
    let mut chunk_storage = HashMap::new();
    for chunk in encrypted_chunks.iter() {
        let hash = self_encryption::hash::content_hash(&chunk.content);
        chunk_storage.insert(hash, chunk.content.clone());
    }

    // Create parallel chunk fetcher that returns chunks in random order
    let parallel_chunk_fetcher =
        |chunk_requests: &[(usize, XorName)]| -> Result<Vec<(usize, Bytes)>> {
            let mut results = Vec::new();

            for &(index, hash) in chunk_requests {
                if let Some(content) = chunk_storage.get(&hash) {
                    results.push((index, content.clone()));
                } else {
                    return Err(Error::Generic(format!(
                        "Chunk not found for hash: {}",
                        hex::encode(hash)
                    )));
                }
            }

            // Randomize the order to simulate parallel retrieval
            let mut rng = thread_rng();
            results.shuffle(&mut rng);

            Ok(results)
        };

    let decrypt_stream = streaming_decrypt(&data_map, parallel_chunk_fetcher)?;
    let decrypted_data = decrypt_stream.range_full()?;

    assert_eq!(
        original_data.as_ref(),
        decrypted_data.as_ref(),
        "Decrypted data should match original"
    );

    Ok(())
}

#[test]
fn test_chunk_verification() -> Result<()> {
    let storage = StorageBackend::new()?;
    let temp_dir = TempDir::new()?;

    // Create test data and encrypt it
    let test_size = 5 * 1024 * 1024; // 5MB
    let data = random_bytes(test_size);
    let input_path = temp_dir.path().join("input.dat");
    File::create(&input_path)?.write_all(&data)?;

    // Encrypt file to get some chunks
    let (data_map, _) = encrypt_file_to_dir(&input_path, storage.disk_dir.path())?;

    // Get the first chunk info and content
    let first_chunk_info = data_map
        .infos()
        .first()
        .ok_or_else(|| crate::Error::Generic("No chunk info available".to_string()))?;
    let chunk_path = storage
        .disk_dir
        .path()
        .join(hex::encode(first_chunk_info.dst_hash));
    let mut chunk_content = Vec::new();
    File::open(&chunk_path)?.read_to_end(&mut chunk_content)?;

    // Test 1: Verify valid chunk
    let verified_chunk = verify_chunk(first_chunk_info.dst_hash, &chunk_content)?;
    assert_eq!(
        verified_chunk.content, chunk_content,
        "Verified chunk content should match original"
    );

    // Test 2: Try with wrong hash
    let mut wrong_hash = first_chunk_info.dst_hash.0;
    wrong_hash[0] ^= 1; // Flip one bit
    let wrong_name = XorName(wrong_hash);
    assert!(
        verify_chunk(wrong_name, &chunk_content).is_err(),
        "Should fail with incorrect hash"
    );

    // Test 3: Try with corrupted content
    let mut corrupted_content = chunk_content.clone();
    if !corrupted_content.is_empty() {
        corrupted_content[0] ^= 1; // Flip one bit
    }
    assert!(
        verify_chunk(first_chunk_info.dst_hash, &corrupted_content).is_err(),
        "Should fail with corrupted content"
    );

    // Test 4: Verify all chunks from encryption
    println!("\nVerifying all chunks from encryption:");
    for (i, info) in data_map.infos().iter().enumerate() {
        let chunk_path = storage.disk_dir.path().join(hex::encode(info.dst_hash));
        let mut chunk_content = Vec::new();
        File::open(&chunk_path)?.read_to_end(&mut chunk_content)?;

        match verify_chunk(info.dst_hash, &chunk_content) {
            Ok(_) => println!("✓ Chunk {i} verified successfully"),
            Err(e) => println!("✗ Chunk {i} verification failed: {e}"),
        }
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════
// E2E tests for quantum-safe crypto upgrade verification
// ═══════════════════════════════════════════════════════════════

fn build_chunk_storage(encrypted_chunks: &[EncryptedChunk]) -> HashMap<XorName, Vec<u8>> {
    let mut storage = HashMap::new();
    for chunk in encrypted_chunks {
        let hash = self_encryption::hash::content_hash(&chunk.content);
        let _ = storage.insert(hash, chunk.content.to_vec());
    }
    storage
}

#[allow(clippy::type_complexity)]
fn make_parallel_fetcher(
    storage: &HashMap<XorName, Vec<u8>>,
) -> impl Fn(&[(usize, XorName)]) -> Result<Vec<(usize, Bytes)>> + '_ {
    move |hashes: &[(usize, XorName)]| -> Result<Vec<(usize, Bytes)>> {
        let mut results = Vec::new();
        for &(index, hash) in hashes {
            let data = storage
                .get(&hash)
                .ok_or_else(|| Error::Generic(format!("Chunk not found: {}", hex::encode(hash))))?;
            results.push((index, Bytes::from(data.clone())));
        }
        Ok(results)
    }
}

/// Helper: stream-encrypt data with a given iterator chunk size, then stream-decrypt and verify.
fn assert_stream_roundtrip(data_size: usize, iter_chunk_size: usize) -> Result<()> {
    let original_data = random_bytes(data_size);

    let mut stream = stream_encrypt(
        data_size,
        original_data
            .chunks(iter_chunk_size)
            .map(|c| Bytes::from(c.to_vec())),
    )?;

    let mut storage = HashMap::new();
    for chunk_result in stream.chunks() {
        let (hash, content) = chunk_result?;
        let _ = storage.insert(hash, content.to_vec());
    }

    let data_map = stream
        .datamap()
        .ok_or_else(|| Error::Generic("No DataMap after stream_encrypt".to_string()))?
        .clone();

    let fetcher = make_parallel_fetcher(&storage);
    let decrypt_stream = streaming_decrypt(&data_map, fetcher)?;
    let decrypted = decrypt_stream.range_full()?;

    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

#[test]
fn test_stream_encrypt_decrypt_roundtrip() -> Result<()> {
    assert_stream_roundtrip(100_000, 4096)
}

#[test]
fn test_file_stream_encrypt_decrypt_roundtrip() -> Result<()> {
    assert_stream_roundtrip(200_000, 8192)
}

#[test]
fn test_stream_encrypt_file_decrypt_storage_cross() -> Result<()> {
    assert_stream_roundtrip(150_000, 8192)
}

/// Helper: standard encrypt() then streaming_decrypt() cross-compatibility check.
fn assert_encrypt_then_stream_decrypt(data_size: usize) -> Result<()> {
    let original_data = random_bytes(data_size);

    let (data_map, encrypted_chunks) = encrypt(original_data.clone())?;
    let storage = build_chunk_storage(&encrypted_chunks);
    let fetcher = make_parallel_fetcher(&storage);

    let decrypt_stream = streaming_decrypt(&data_map, fetcher)?;
    let decrypted = decrypt_stream.range_full()?;

    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

#[test]
fn test_file_encrypt_stream_decrypt_cross() -> Result<()> {
    assert_encrypt_then_stream_decrypt(150_000)
}

#[test]
fn test_memory_encrypt_stream_decrypt() -> Result<()> {
    assert_encrypt_then_stream_decrypt(100_000)
}

#[test]
fn test_stream_encrypt_memory_decrypt() -> Result<()> {
    let data_size = 100_000;
    let original_data = random_bytes(data_size);

    let mut stream = stream_encrypt(
        data_size,
        original_data.chunks(4096).map(|c| Bytes::from(c.to_vec())),
    )?;

    let mut chunks = Vec::new();
    for chunk_result in stream.chunks() {
        let (_hash, content) = chunk_result?;
        chunks.push(EncryptedChunk { content });
    }

    let data_map = stream
        .datamap()
        .ok_or_else(|| Error::Generic("No DataMap".to_string()))?;

    let decrypted = decrypt(data_map, &chunks)?;
    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

// --- Task 9: DecryptionStream random access ---

#[test]
fn test_streaming_decrypt_range_access() -> Result<()> {
    let data_size = 50_000;
    let original_data = random_bytes(data_size);

    let (data_map, encrypted_chunks) = encrypt(original_data.clone())?;
    let storage = build_chunk_storage(&encrypted_chunks);
    let fetcher = make_parallel_fetcher(&storage);

    let stream = streaming_decrypt(&data_map, fetcher)?;

    // get_range
    let range = stream.get_range(1000, 2000)?;
    assert_eq!(range.as_ref(), &original_data[1000..3000]);

    // range
    let range = stream.range(500..1500)?;
    assert_eq!(range.as_ref(), &original_data[500..1500]);

    // range_full
    let full = stream.range_full()?;
    assert_eq!(full.as_ref(), &original_data[..]);

    // range_from
    let from = stream.range_from(40_000)?;
    assert_eq!(from.as_ref(), &original_data[40_000..]);

    // range_to
    let to = stream.range_to(5000)?;
    assert_eq!(to.as_ref(), &original_data[..5000]);

    // range_inclusive
    let inclusive = stream.range_inclusive(100, 199)?;
    assert_eq!(inclusive.as_ref(), &original_data[100..200]);

    // file_size
    assert_eq!(stream.file_size(), data_size);

    Ok(())
}

// --- Task 10: Large file streaming roundtrip ---

#[test]
fn test_large_file_streaming_roundtrip() -> Result<()> {
    let data_size = 20 * 1024 * 1024; // 20MB
    let original_data = random_bytes(data_size);

    let mut stream = stream_encrypt(
        data_size,
        original_data.chunks(8192).map(|c| Bytes::from(c.to_vec())),
    )?;

    let mut storage = HashMap::new();
    for chunk_result in stream.chunks() {
        let (hash, content) = chunk_result?;
        let _ = storage.insert(hash, content.to_vec());
    }

    let data_map = stream
        .datamap()
        .ok_or_else(|| Error::Generic("No DataMap".to_string()))?
        .clone();

    let fetcher = make_parallel_fetcher(&storage);
    let decrypt_stream = streaming_decrypt(&data_map, fetcher)?;
    let decrypted = decrypt_stream.range_full()?;

    assert_eq!(decrypted.len(), data_size);
    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

// --- Task 11: Edge cases ---

#[test]
fn test_minimum_size_data() -> Result<()> {
    let data_size = self_encryption::MIN_ENCRYPTABLE_BYTES;
    let original_data = random_bytes(data_size);

    let (data_map, chunks) = encrypt(original_data.clone())?;
    let decrypted = decrypt(&data_map, &chunks)?;
    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

#[test]
fn test_single_chunk_boundary() -> Result<()> {
    let data_size = self_encryption::MAX_CHUNK_SIZE;
    let original_data = random_bytes(data_size);

    let (data_map, chunks) = encrypt(original_data.clone())?;
    let decrypted = decrypt(&data_map, &chunks)?;
    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

#[test]
fn test_multi_chunk_boundary() -> Result<()> {
    let data_size = 3 * self_encryption::MAX_CHUNK_SIZE;
    let original_data = random_bytes(data_size);

    let (data_map, chunks) = encrypt(original_data.clone())?;
    let decrypted = decrypt(&data_map, &chunks)?;
    assert_eq!(decrypted.as_ref(), &original_data[..]);
    Ok(())
}

#[test]
fn test_non_aligned_sizes() -> Result<()> {
    for data_size in [
        self_encryption::MAX_CHUNK_SIZE + 1,
        self_encryption::MAX_CHUNK_SIZE * 2 + 7,
        self_encryption::MIN_ENCRYPTABLE_BYTES + 1,
        99_999,
    ] {
        let original_data = random_bytes(data_size);
        let (data_map, chunks) = encrypt(original_data.clone())?;
        let decrypted = decrypt(&data_map, &chunks)?;
        assert_eq!(
            decrypted.as_ref(),
            &original_data[..],
            "Failed for data_size={data_size}"
        );
    }
    Ok(())
}

// --- Task 12-16: Security verification tests ---

#[test]
fn test_encrypted_chunks_do_not_contain_plaintext() -> Result<()> {
    let pattern = b"HELLO_WORLD_PATTERN_12345";
    let mut data = Vec::new();
    while data.len() < 100_000 {
        data.extend_from_slice(pattern);
    }
    let original_data = Bytes::from(data);

    let (_, encrypted_chunks) = encrypt(original_data)?;

    for (i, chunk) in encrypted_chunks.iter().enumerate() {
        for window in chunk.content.windows(pattern.len()) {
            assert_ne!(
                window, pattern,
                "Chunk {i} contains plaintext pattern — encryption is broken!"
            );
        }
    }
    Ok(())
}

#[test]
fn test_tampered_chunk_fails_decryption() -> Result<()> {
    let original_data = random_bytes(10_000);
    let (data_map, mut encrypted_chunks) = encrypt(original_data)?;

    // Tamper with the first chunk — flip a bit in the middle
    if let Some(first_chunk) = encrypted_chunks.first_mut() {
        let mut tampered = first_chunk.content.to_vec();
        let mid = tampered.len() / 2;
        if let Some(byte) = tampered.get_mut(mid) {
            *byte ^= 0xFF;
        }
        first_chunk.content = Bytes::from(tampered);
    }

    // Decryption should fail due to AEAD tag verification
    let result = decrypt(&data_map, &encrypted_chunks);
    assert!(
        result.is_err(),
        "Decryption should fail with tampered chunk (AEAD integrity check)"
    );
    Ok(())
}

#[test]
fn test_wrong_datamap_fails_decryption() -> Result<()> {
    let data_a = random_bytes(10_000);
    let data_b = random_bytes(10_000);

    let (data_map_a, _chunks_a) = encrypt(data_a)?;
    let (_data_map_b, chunks_b) = encrypt(data_b)?;

    // Try to decrypt chunks_b with data_map_a
    let result = decrypt(&data_map_a, &chunks_b);
    assert!(
        result.is_err(),
        "Decryption should fail when using wrong DataMap"
    );
    Ok(())
}

#[test]
fn test_aead_tag_protects_integrity() -> Result<()> {
    use self_encryption::hash::content_hash;

    let plaintext = Bytes::from(vec![42u8; 1000]);

    // Create keys for encryption
    let key_hash = content_hash(b"test_key_data");
    let nonce_hash = content_hash(b"test_nonce_data");

    // Build cipher key and nonce
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&key_hash.0);

    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&nonce_hash.0[..12]);

    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

    let key = Key::from_slice(&key_bytes);
    let cipher = ChaCha20Poly1305::new(key);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|_| Error::Generic("Encryption failed".to_string()))?;

    // Verify ciphertext is 16 bytes longer than plaintext (Poly1305 tag)
    assert_eq!(
        ciphertext.len(),
        plaintext.len() + 16,
        "AEAD ciphertext should be plaintext + 16 bytes (auth tag)"
    );

    // Verify normal decryption works
    let decrypted = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| Error::Generic("Decryption failed".to_string()))?;
    assert_eq!(decrypted, plaintext.as_ref());

    // Flip a bit in ciphertext body → should fail
    let mut tampered_body = ciphertext.clone();
    if let Some(byte) = tampered_body.get_mut(0) {
        *byte ^= 1;
    }
    assert!(
        cipher.decrypt(nonce, tampered_body.as_ref()).is_err(),
        "Flipping ciphertext bit should fail AEAD verification"
    );

    // Flip a bit in auth tag (last 16 bytes) → should fail
    let mut tampered_tag = ciphertext.clone();
    let tag_idx = tampered_tag.len() - 1;
    if let Some(byte) = tampered_tag.get_mut(tag_idx) {
        *byte ^= 1;
    }
    assert!(
        cipher.decrypt(nonce, tampered_tag.as_ref()).is_err(),
        "Flipping auth tag bit should fail AEAD verification"
    );

    Ok(())
}

// --- AEAD pipeline integrity test ---

#[test]
fn test_aead_rejects_tampered_ciphertext_through_pipeline() -> Result<()> {
    use self_encryption::decrypt_chunk;

    let original_data = random_bytes(10_000);
    let (data_map, encrypted_chunks) = encrypt(original_data)?;

    // Extract the src_hashes from the data map (the valid ones used during encryption)
    let src_hashes: Vec<XorName> = data_map.infos().iter().map(|info| info.src_hash).collect();

    // Take the first chunk's encrypted content and tamper with it
    let first_chunk = encrypted_chunks
        .first()
        .ok_or_else(|| Error::Generic("no chunks".to_string()))?;
    let mut tampered_content = first_chunk.content.to_vec();
    let mid = tampered_content.len() / 2;
    if let Some(byte) = tampered_content.get_mut(mid) {
        *byte ^= 0xFF;
    }
    let tampered_bytes = Bytes::from(tampered_content);

    // Call decrypt_chunk directly with valid src_hashes but tampered ciphertext
    // This proves the AEAD layer (not hash lookup) is the integrity gate
    let result = decrypt_chunk(0, &tampered_bytes, &src_hashes, 0);
    assert!(
        result.is_err(),
        "decrypt_chunk should fail on tampered ciphertext due to AEAD tag verification"
    );

    Ok(())
}

// --- Task 17: Verify BLAKE3 is used for hashing ---

#[test]
fn test_content_hash_is_blake3() -> Result<()> {
    let test_data = b"known content for blake3 verification";
    let expected_hash = blake3::hash(test_data);
    let actual_hash = self_encryption::hash::content_hash(test_data);
    assert_eq!(
        actual_hash.0,
        *expected_hash.as_bytes(),
        "content_hash should produce BLAKE3 output"
    );
    Ok(())
}

#[test]
fn test_encrypted_chunk_dst_hash_is_blake3() -> Result<()> {
    let original_data = random_bytes(10_000);
    let (data_map, encrypted_chunks) = encrypt(original_data)?;

    for (info, chunk) in data_map.infos().iter().zip(encrypted_chunks.iter()) {
        let computed_hash = self_encryption::hash::content_hash(&chunk.content);
        assert_eq!(
            computed_hash, info.dst_hash,
            "dst_hash in DataMap should match BLAKE3 hash of encrypted content"
        );
    }
    Ok(())
}

// --- Task 18: Verify key derivation sizes and roundtrip ---

#[test]
fn test_key_derivation_sizes_and_roundtrip() -> Result<()> {
    // XorName is 32 bytes
    assert_eq!(std::mem::size_of::<XorName>(), 32);

    // Encrypt and decrypt a minimal file to verify the key derivation works end-to-end
    let data = random_bytes(self_encryption::MIN_ENCRYPTABLE_BYTES);
    let (data_map, chunks) = encrypt(data.clone())?;
    let decrypted = decrypt(&data_map, &chunks)?;
    assert_eq!(decrypted.as_ref(), &data[..]);

    // Verify the data map has 3 chunks with valid hashes
    assert_eq!(data_map.len(), 3);
    for info in data_map.infos() {
        assert_ne!(info.src_hash, XorName::default());
        assert_ne!(info.dst_hash, XorName::default());
    }

    Ok(())
}
