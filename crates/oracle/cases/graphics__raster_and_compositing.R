library(grid)
directory <- tempfile()
dir.create(directory)
# The colours of an uncompressed bitmap's pixels, as "#RRGGBB" strings from the bottom row up.
# GNU R writes an 8-bit bitmap with a palette and ry a 24-bit one, so both layouts are read.
bitmap_colours <- function(file) {
  bytes <- readBin(file, "raw", file.size(file))
  word <- function(at, size) readBin(bytes[at + seq_len(size)], "integer", size = size,
                                     endian = "little", signed = size == 4)
  offset <- word(10, 4)
  width <- word(18, 4)
  height <- word(22, 4)
  depth <- word(28, 2)
  stride <- 4 * ceiling(width * depth / 32)
  rows <- lapply(seq_len(height) - 1, function(row) {
    start <- offset + row * stride
    if (depth == 8) {
      index <- as.integer(bytes[start + seq_len(width)])
      palette <- 54 + 4 * index
      cbind(bytes[palette + 3], bytes[palette + 2], bytes[palette + 1])
    } else {
      pixel <- start + 3 * (seq_len(width) - 1)
      cbind(bytes[pixel + 3], bytes[pixel + 2], bytes[pixel + 1])
    }
  })
  colours <- do.call(rbind, rows)
  paste0("#", apply(colours, 1, paste, collapse = ""))
}
image <- matrix(c("red", "blue", rgb(0, 1, 0, 0.5), "white"), 2)
raster <- function(interpolate) {
  par(mar = c(0, 0, 0, 0))
  plot.new()
  rasterImage(image, 0, 0, 1, 1, interpolate = interpolate)
}
file <- file.path(directory, "raster.bmp")
bmp(file, 60, 60)
raster(FALSE)
invisible(dev.off())
# Edge pixels are anti-aliased by coverage, which differs slightly between cairo and ry's
# rasterizer, so only the interior colours are compared.
counts <- table(bitmap_colours(file))
print(counts[counts > 100])
file <- file.path(directory, "saturate.bmp")
bmp(file, 60, 60)
grid.group(rectGrob(width = 0.6, height = 0.6, gp = gpar(fill = rgb(0, 0, 1, 0.6), col = NA)),
           "saturate",
           rectGrob(x = 0.3, y = 0.3, width = 0.4, height = 0.4,
                    gp = gpar(fill = rgb(1, 0, 0, 0.7), col = NA)))
invisible(dev.off())
print(table(bitmap_colours(file)))
for (interpolate in c(FALSE, TRUE)) {
  file <- file.path(directory, "raster.ps")
  postscript(file, width = 2, height = 2)
  raster(interpolate)
  invisible(dev.off())
  lines <- readLines(file)
  start <- grep("image$", lines)[1]
  after <- lines[-seq_len(start)]
  data <- after[seq_len(which(!grepl("^[0-9a-f>]+$", after))[1] - 1)]
  data <- sub(">$", "", paste(data, collapse = ""))
  writeLines(data, hex <- file.path(directory, "hex.txt"))
  cat("postscript", interpolate, nchar(data), unname(tools::md5sum(hex)), "\n")
}
file <- file.path(directory, "raster.pdf")
pdf(file)
raster(TRUE)
invisible(dev.off())
bytes <- readBin(file, "raw", file.size(file))
pdf <- rawToChar(bytes[bytes != as.raw(0)])
cat("pdf", grepl("/Interpolate true", pdf, fixed = TRUE, useBytes = TRUE), grepl("/SMask", pdf, fixed = TRUE, useBytes = TRUE), "\n")
print(typeof(cbind(as.raw(1:2), as.raw(3:4))))
