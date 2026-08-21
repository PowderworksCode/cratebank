variable "cloudflare_api_token" {
  description = "API token with Pipelines, R2 and DNS edit permissions"
  type        = string
  sensitive   = true
}

variable "account_id" {
  description = "Cloudflare account id"
  type        = string
}

variable "zone_id" {
  description = "Zone id for cratebank.io. Leave empty to skip the CNAME and use the raw Cloudflare endpoint -- useful for a first test apply before the domain is set up."
  type        = string
  default     = ""
}

variable "zone_name" {
  description = "Apex domain, e.g. cratebank.io"
  type        = string
  default     = "cratebank.io"
}

variable "bucket_name" {
  type    = string
  default = "cratebank"
}

variable "bucket_location" {
  description = "R2 location hint, e.g. WNAM, ENAM, WEUR, EEUR, APAC"
  type        = string
  default     = "ENAM"
}

# R2 credentials for the sink. Kept as variables rather than created with
# cloudflare_api_token so the secret does not land in Terraform state.
variable "r2_access_key_id" {
  type      = string
  sensitive = true
}

variable "r2_secret_access_key" {
  type      = string
  sensitive = true
}
