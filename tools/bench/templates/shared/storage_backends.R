#@ weight 3
#@ param BLOBS int 1 8
# Cloud and database access behind an injectable client, so tests never touch a network.
#: @alias BlobClient@ID@ {list{list: fn(container: character) -> data.frame | character[], read: fn(container: character, name: character) -> raw[] | character | NULL}}

#: fn(account: character, [client]: BlobClient@ID@ | NULL) -> BlobClient@ID@
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

#: fn(vault_url: character, names: character[], [client]: (fn(name: character) -> character) | NULL) -> list[character]
load_secrets_@ID@ <- function(vault_url, names, client = NULL) {
  if (is.null(client)) {
    vault <- AzureKeyVault::key_vault(vault_url)
    #: fn(name: character) -> character
    client <- function(name) {
      #: @if-unknown list{get: fn(name: character) -> list{value: character}}
      secrets <- vault$secrets
      secrets$get(name)$value
    }
  }
  stats::setNames(lapply(names, client), names)
}

#: <T> fn(endpoint_url: character, row: list[named: Any], [client]: (fn(entity: list[named: Any]) -> T) | NULL) -> T | character | NULL
append_table_row_@ID@ <- function(endpoint_url, row, client = NULL) {
  if (is.null(client)) {
    endpoint <- AzureTableStor::table_endpoint(endpoint_url)
    #: fn(entity: list[named: Any]) -> character | NULL
    client <- function(entity) AzureTableStor::insert_table_entity(AzureTableStor::storage_table(endpoint, "log"), entity)
  }
  client(row)
}

#: @pure
#: fn(table: character, columns: character[], [limit]: integer) -> character
query_text_@ID@ <- function(table, columns, limit = @BLOBS@L) {
  con <- DBI::ANSI()
  sprintf(
    "SELECT %s FROM %s LIMIT %d",
    paste(DBI::dbQuoteIdentifier(con, columns), collapse = ", "),
    DBI::dbQuoteIdentifier(con, table),
    limit
  )
}

#: fn(dsn: character) -> Pool
pool_connector_@ID@ <- function(dsn) pool::dbPool(odbc::odbc(), dsn = dsn)

#: <T> fn(dsn: character, [connector]: fn(dsn: character) -> T) -> T
connect_database_@ID@ <- function(dsn, connector = pool_connector_@ID@) {
  connector(dsn)
}
#@ test
test_that("storage backends @ID@ use the injected client", {
  fake <- list(list = function(container) paste0("blob", seq_len(@BLOBS@)), read = function(container, name) name)
  client <- open_storage_@ID@("account", fake)
  expect_length(client$list("c"), @BLOBS@)
  secrets <- load_secrets_@ID@("https://vault.test", c("a", "b"), client = function(name) toupper(name))
  expect_equal(secrets$b, "B")
  #: fn(row: list[named: double]) -> double | NULL
  first_value <- function(row) row$x
  expect_equal(append_table_row_@ID@("https://t.test", list(x = 1), client = first_value), 1)
  expect_equal(query_text_@ID@("trades", c("id", "price")), "SELECT \"id\", \"price\" FROM \"trades\" LIMIT @BLOBS@")
  expect_equal(connect_database_@ID@("dsn", connector = function(dsn) paste("connected", dsn)), "connected dsn")
})
