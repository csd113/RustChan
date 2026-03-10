#!/bin/bash

set -o pipefail

OUTPUT_DIR="clippy_reports"
RAW_FILE="$OUTPUT_DIR/clippy_raw.txt"
CLUSTER_DIR="$OUTPUT_DIR/clusters"

mkdir -p "$OUTPUT_DIR"
mkdir -p "$CLUSTER_DIR"

echo "Running cargo clippy..."

cargo clippy --all-targets --all-features -- \
-D warnings \
-W clippy::pedantic \
-W clippy::nursery \
2>&1 | tee "$RAW_FILE"

echo "Parsing Clippy output..."

while IFS= read -r line
do
	# Extract file path patterns like src/db/file.rs:12:5
	if [[ $line =~ ([a-zA-Z0-9_\/.-]+\.rs):[0-9]+:[0-9]+ ]]; then
		
		FILE="${BASH_REMATCH[1]}"

		# Determine folder cluster
		DIR=$(dirname "$FILE")

		# Normalize root files into their own cluster
		if [[ "$DIR" == "." ]]; then
			CLUSTER="root"
		else
			CLUSTER=$(echo "$DIR" | tr '/' '_')
		fi

		OUTFILE="$CLUSTER_DIR/${CLUSTER}.txt"

		echo "" >> "$OUTFILE"
		echo "----------------------------------------" >> "$OUTFILE"
		echo "FILE: $FILE" >> "$OUTFILE"
		echo "----------------------------------------" >> "$OUTFILE"
	fi

	# Append the line to the current cluster if defined
	if [[ -n "$OUTFILE" ]]; then
		echo "$line" >> "$OUTFILE"
	fi

done < "$RAW_FILE"

echo "Cluster reports generated in:"
echo "$CLUSTER_DIR"