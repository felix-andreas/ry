#@ weight 3
#@ param BLOBS int 1 8
# Cloud and database access behind an injectable client, so tests never touch a network.
open_storage_@ID@ <- function(account, client = NULL) {
  if (is.null(client)) {
    token <- AzureAuth::get_azure_token("https://storage.azure.com", tenant = "common", app = account)
    endpoint <- AzureStor::storage_endpoint(paste0("https://", account, ".blob.core.windows.net"), token = token)
    client <- list(
      list = function(container) AzureStor::list_blobs(AzureStor::storage_container(endpoint, container)),
      read = function(container, name) AzureStor::storage_download(AzureStor::storage_container(endpoint, container), name, NULL)
    )
  }
  client
}

load_secrets_@ID@ <- function(vault_url, names, client = NULL) {
  if (is.null(client)) {
    vault <- AzureKeyVault::key_vault(vault_url)
    client <- function(name) vault$secrets$get(name)$value
  }
  stats::setNames(lapply(names, client), names)
}

append_table_row_@ID@ <- function(endpoint_url, row, client = NULL) {
  if (is.null(client)) {
    endpoint <- AzureTableStor::table_endpoint(endpoint_url)
    client <- function(entity) AzureTableStor::insert_table_entity(AzureTableStor::storage_table(endpoint, "log"), entity)
  }
  client(row)
}

query_text_@ID@ <- function(table, columns, limit = @BLOBS@L) {
  con <- DBI::ANSI()
  sprintf(
    "SELECT %s FROM %s LIMIT %d",
    paste(DBI::dbQuoteIdentifier(con, columns), collapse = ", "),
    DBI::dbQuoteIdentifier(con, table),
    limit
  )
}

connect_database_@ID@ <- function(dsn, connector = function(dsn) pool::dbPool(odbc::odbc(), dsn = dsn)) {
  connector(dsn)
}
#@ test
test_that("storage backends @ID@ use the injected client", {
  fake <- list(list = function(container) paste0("blob", seq_len(@BLOBS@)), read = function(container, name) name)
  client <- open_storage_@ID@("account", fake)
  expect_length(client$list("c"), @BLOBS@)
  secrets <- load_secrets_@ID@("https://vault.test", c("a", "b"), client = function(name) toupper(name))
  expect_equal(secrets$b, "B")
  expect_equal(append_table_row_@ID@("https://t.test", list(x = 1), client = function(row) row$x), 1)
  expect_equal(query_text_@ID@("trades", c("id", "price")), "SELECT \"id\", \"price\" FROM \"trades\" LIMIT @BLOBS@")
  expect_equal(connect_database_@ID@("dsn", connector = function(dsn) paste("connected", dsn)), "connected dsn")
})
