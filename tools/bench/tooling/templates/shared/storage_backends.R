#@ weight 3
#@ param BLOBS int 1 8
# Cloud and database access behind an injectable client, so tests never touch a network.
alias BlobClient@ID@ = list{list: fn(container: character) -> data.frame | character[], read: fn(container: character, name: character) -> raw[] | character | NULL}

open_storage_@ID@ <- function(account: character, client: BlobClient@ID@ | NULL = NULL) -> BlobClient@ID@ {
  if (is.null(client)) {
    token <- AzureAuth::get_azure_token("https://storage.azure.com", tenant = "common", app = account)
    endpoint <- AzureStor::storage_endpoint(paste0("https://", account, ".blob.core.windows.net"), token = token)
    client <- list(
      list = function(container: character) -> data.frame | character[] AzureStor::list_blobs(AzureStor::storage_container(endpoint, container)),
      read = function(container: character, name: character) -> raw[] | NULL AzureStor::storage_download(AzureStor::storage_container(endpoint, container), name, NULL)
    )
  }
  client
}

load_secrets_@ID@ <- function(vault_url: character, names: character[], client: (fn(name: character) -> character) | NULL = NULL) -> list[character] {
  if (is.null(client)) {
    vault <- AzureKeyVault::key_vault(vault_url)
    client <- function(name: character) -> character {
      secrets: @if-unknown list{get: fn(name: character) -> list{value: character}} <- vault$secrets
      secrets$get(name)$value
    }
  }
  stats::setNames(lapply(names, client), names)
}

append_table_row_@ID@ <- function<T>(endpoint_url: character, row: list[named: Any], client: (fn(entity: list[named: Any]) -> T) | NULL = NULL) -> T | character | NULL {
  if (is.null(client)) {
    endpoint <- AzureTableStor::table_endpoint(endpoint_url)
    client <- function(entity: list[named: Any]) -> character | NULL AzureTableStor::insert_table_entity(AzureTableStor::storage_table(endpoint, "log"), entity)
  }
  client(row)
}

query_text_@ID@ <- @pure function(table: character, columns: character[], limit: integer = @BLOBS@L) -> character {
  con <- DBI::ANSI()
  sprintf(
    "SELECT %s FROM %s LIMIT %d",
    paste(DBI::dbQuoteIdentifier(con, columns), collapse = ", "),
    DBI::dbQuoteIdentifier(con, table),
    limit
  )
}

pool_connector_@ID@ <- function(dsn: character) -> Pool pool::dbPool(odbc::odbc(), dsn = dsn)

connect_database_@ID@ <- function<T>(dsn: character, connector: fn(dsn: character) -> T = pool_connector_@ID@) -> T {
  connector(dsn)
}
#@ test
test_that("storage backends @ID@ use the injected client", {
  fake <- list(list = function(container: character) -> character[] paste0("blob", seq_len(@BLOBS@)), read = function(container: character, name: character) -> character name)
  client <- open_storage_@ID@("account", fake)
  expect_length(client$list("c"), @BLOBS@)
  secrets <- load_secrets_@ID@("https://vault.test", c("a", "b"), client = function(name: character) -> character toupper(name))
  expect_equal(secrets$b, "B")
  first_value <- function(row: list[named: double]) -> double | NULL row$x
  expect_equal(append_table_row_@ID@("https://t.test", list(x = 1), client = first_value), 1)
  expect_equal(query_text_@ID@("trades", c("id", "price")), "SELECT \"id\", \"price\" FROM \"trades\" LIMIT @BLOBS@")
  expect_equal(connect_database_@ID@("dsn", connector = function(dsn: character) -> character paste("connected", dsn)), "connected dsn")
})
