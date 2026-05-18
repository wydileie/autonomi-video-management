export interface AuthState {
  expires_at?: string;
  refresh_token_expires_at?: string;
  username?: string;
}

export interface LoginCredentials {
  username: string;
  password: string;
}

export interface CurrentUser {
  username: string;
}

export interface ResolutionOption {
  value: string;
  label: string;
  width: number;
  height: number;
  bitrate: string;
  note: string;
}

export interface SourceVideoMeta {
  loading?: boolean;
  width: number | null;
  height: number | null;
  duration?: number | null;
  size?: number;
}

export interface VideoSummary {
  created_at: string;
  description?: string | null;
  id: string;
  is_public?: boolean;
  original_filename?: string | null;
  status: string;
  title: string;
}

export interface VideoVariant {
  id: string;
  resolution: string;
  segment_count?: number;
}

export interface QuoteOriginalFile {
  byte_size?: number;
  estimated_bytes?: number;
}

export interface UploadQuote {
  actual_media_bytes?: number;
  actual_transcoded_bytes?: number;
  approval_expires_at?: string | null;
  estimated_bytes: number;
  estimated_gas_cost_wei: string;
  metadata_bytes?: number;
  original_file?: QuoteOriginalFile | null;
  payment_mode: string;
  sampled?: boolean;
  segment_count: number;
  storage_cost_atto: string;
}

export interface VideoDetail extends VideoSummary {
  approval_expires_at?: string | null;
  error_message?: string | null;
  final_quote?: UploadQuote | null;
  manifest_address?: string | null;
  original_file_address?: string | null;
  show_manifest_address?: boolean;
  show_original_filename?: boolean;
  variants: VideoVariant[];
}

export interface CatalogVariantSummary {
  height: number;
  resolution: string;
  segment_count: number;
  total_duration?: number | null;
  width: number;
}

export interface CatalogVideoSummary {
  created_at: string;
  description?: string | null;
  id: string;
  is_public: boolean;
  manifest_address: string;
  status: string;
  title: string;
  updated_at: string;
  variants: CatalogVariantSummary[];
}

export interface PortableCatalogDocument {
  catalog_kind: "published" | "all";
  content_type: string;
  generated_at: string;
  schema_version: number;
  updated_at: string;
  videos: CatalogVideoSummary[];
}

export interface AdminCatalogs {
  all_catalog: PortableCatalogDocument | null;
  all_catalog_address: string | null;
  published_catalog: PortableCatalogDocument | null;
  published_catalog_address: string | null;
}

export interface UploadQuoteRequest {
  duration_seconds: number;
  resolutions: string[];
  source_height: number | null;
  source_width: number | null;
  source_size_bytes?: number;
  upload_original?: boolean;
}

export interface VisibilityUpdate {
  show_manifest_address: boolean;
  show_original_filename: boolean;
}
