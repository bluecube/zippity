use std::collections::HashMap;

use indexmap::IndexMap;

use crate::{Builder, Reader};
use bytes::Bytes;
use proptest::{
    arbitrary::Arbitrary,
    strategy::{BoxedStrategy, Just, MapInto, Strategy},
};

#[derive(Clone, Debug)]
pub struct TestEntryData(pub IndexMap<String, Bytes>);

/// A type that holds both a reader and a hash map with the entries the reader contains.
#[derive(Clone, Debug)]
pub struct ReaderAndData {
    pub reader: Reader<Bytes>,
    pub data: TestEntryData,
}

#[derive(Clone, Debug)]
pub struct ArbitraryTestEntryDataParams {
    /// How many entries to generate at most
    pub max_count: usize,

    /// How much data can an entry have
    pub max_size: usize,
}

#[derive(Clone, Debug, Default)]
pub struct ArbitraryReaderParams {
    /// Parameters for generating the entries
    pub entries: ArbitraryTestEntryDataParams,
    /// Controls wheter the arbitrary implementation should also seek the reader to random position.
    pub seek: bool,
}

impl Arbitrary for TestEntryData {
    type Parameters = ArbitraryTestEntryDataParams;
    type Strategy = BoxedStrategy<TestEntryData>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        proptest::collection::hash_map(
            ".*",
            proptest::collection::vec(proptest::bits::u8::ANY, 0..args.max_size)
                .prop_map_into::<Bytes>(),
            0..args.max_count,
        )
        .prop_map_into()
        .boxed()
    }
}

impl Arbitrary for ReaderAndData {
    type Parameters = ArbitraryReaderParams;
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        let fresh_reader_strategy =
            TestEntryData::arbitrary_with(args.clone().into()).prop_map(|data| {
                let builder: Builder<Bytes> = data.clone().into();

                ReaderAndData {
                    reader: builder.build(),
                    data,
                }
            });

        if args.seek {
            fresh_reader_strategy
                .prop_flat_map(|reader_and_data| {
                    let len = reader_and_data.reader.size();
                    (Just(reader_and_data), 0..len)
                })
                .prop_map(|(mut reader_and_data, seek_offset)| {
                    reader_and_data.reader.seek_from_start_mut(seek_offset);
                    reader_and_data
                })
                .boxed()
        } else {
            fresh_reader_strategy.boxed()
        }
    }
}

impl Arbitrary for Reader<Bytes> {
    type Parameters = ArbitraryReaderParams;
    type Strategy = MapInto<BoxedStrategy<ReaderAndData>, Self>;

    fn arbitrary_with(args: Self::Parameters) -> Self::Strategy {
        ReaderAndData::arbitrary_with(args).prop_map_into()
    }
}

impl Default for ArbitraryTestEntryDataParams {
    fn default() -> Self {
        ArbitraryTestEntryDataParams {
            max_count: 64,
            max_size: 512,
        }
    }
}

impl From<ReaderAndData> for Reader<Bytes> {
    fn from(value: ReaderAndData) -> Self {
        value.reader
    }
}

impl From<HashMap<String, Bytes>> for TestEntryData {
    fn from(value: HashMap<String, Bytes>) -> Self {
        TestEntryData(value.into_iter().collect())
    }
}

impl From<TestEntryData> for Builder<Bytes> {
    fn from(value: TestEntryData) -> Self {
        let mut builder: Builder<Bytes> = Builder::new();

        value.0.into_iter().for_each(|(name, content)| {
            builder
                .add_entry_with_size(name.clone(), content.clone(), content.len() as u64)
                .expect("Adding entries from hash map should never fail");
        });

        builder
    }
}

impl From<ArbitraryReaderParams> for ArbitraryTestEntryDataParams {
    fn from(value: ArbitraryReaderParams) -> Self {
        value.entries
    }
}
