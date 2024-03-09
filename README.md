# Wikigraph
Converts Wikipedia's XML Database dumps into a graph stored in a binary format. Inspired by: Tristan Hume's [Wikicrush](https://github.com/trishume/wikicrush). This borrows the binary format that Tristan described in the Readme of Wikicrush, which is highly compact and compresses the almost 100GB Wikipedia XML dump into a ~ 1GB Binary link graph. 
## File format:
As mentioned above, I'm using the file format that Trisan Hume described. It contains a File header, a page header, and the links. Each header is represented by 4 32-bit integers. The file header has 2 unused integers, 1 integer representing the version, and 1 integer representing the number of pages (also called node in my code). The page header contains 3 unused integers which are used for marking visited nodes in traversal, as well as the number of links that the page has. Each link is a single integer that contains the byteoffset of the page it is linking to. This lets you skip to the next page by incrementing (4 * num_links) bytes forward. This also lets you easily access the page that is linked by moving the reader to the byteoffset. 
## How it works:
The script runs in 2 sections. The first section, it uses [quick_xml](https://docs.rs/quick-xml/latest/quick_xml/) to read through the dump and tries to parse all of the valid links from each page. It will append this data into a text adjacency list, which is used later on to reconstruct the binary graph. It also computes the byteoffsets and lengths of each valid page and stores it in a postgres database. For example: Anarchism has a byteoffset of 16 and a length of 1536 (all in bytes). This can be interpreted as: Anarchism is 16 bytes from the start of the file and has 1536/4 = 384 links. After this pre-processing stage is finished, The graph will get constructed from the adjacency list, using the postgres database as a lookup table for the byteoffsets. The completed .bin file can be traversed by adapting any pathfinding algorithim to the file format. 

## Performance:
All runs were performed in a docker environment using 8gbs of ram and 6 M2 CPU cores. 
Speed results for pre-processing section:
- 12 hours using regex to extract links
- 7 hours using custom parsing function
Speed results for graph-building section:
- //todo: Run this
