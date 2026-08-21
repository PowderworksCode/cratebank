variable "cloudflare_api_token" {
  description = "API token with Workers Scripts, R2, Workers Routes and DNS edit permissions"
  type        = string
  sensitive   = true
}

variable "account_id" {
  description = "Cloudflare account id"
  type        = string
}

variable "zone_id" {
  description = "Zone id for cratebank.io. Leave empty to skip the Workers custom domain and use the workers.dev subdomain -- useful for a first test apply before the domain is set up."
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

# Out-of-band R2 access (aws-cli, rclone). The Worker does not use these -- it
# reaches the bucket through a binding. Kept as variables rather than minted by
# Terraform so the token that runs this cannot also create credentials.
variable "r2_access_key_id" {
  type      = string
  sensitive = true
}

variable "r2_secret_access_key" {
  type      = string
  sensitive = true
}
