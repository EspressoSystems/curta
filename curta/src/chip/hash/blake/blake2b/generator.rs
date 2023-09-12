use core::marker::PhantomData;

use itertools::Itertools;
use plonky2::field::extension::Extendable;
use plonky2::hash::hash_types::RichField;
use plonky2::iop::generator::{GeneratedValues, SimpleGenerator};
use plonky2::iop::target::Target;
use plonky2::iop::witness::{PartitionWitness, Witness, WitnessWrite};
use plonky2::plonk::circuit_builder::CircuitBuilder;
use plonky2::plonk::circuit_data::CommonCircuitData;
use plonky2::util::serialization::{Buffer, Read, Write};

use super::{BLAKE2BPublicData, INITIAL_HASH};
use crate::chip::hash::blake::blake2b::BLAKE2BGadget;
use crate::chip::trace::generator::ArithmeticGenerator;
use crate::chip::uint::bytes::lookup_table::table::ByteLookupTable;
use crate::chip::uint::operations::instruction::U32Instruction;
use crate::chip::uint::util::u64_to_le_field_bytes;
use crate::chip::AirParameters;
use crate::math::field::PrimeField64;
use crate::math::prelude::CubicParameters;

#[derive(Debug, Clone, Copy)]
pub struct BLAKE2BAirParameters<F, E>(pub PhantomData<(F, E)>);

impl<F: PrimeField64, E: CubicParameters<F>> AirParameters for BLAKE2BAirParameters<F, E> {
    type Field = F;
    type CubicParams = E;

    type Instruction = U32Instruction;

    const NUM_FREE_COLUMNS: usize = 551;
    const EXTENDED_COLUMNS: usize = 927;
    const NUM_ARITHMETIC_COLUMNS: usize = 0;

    fn num_rows_bits() -> usize {
        16
    }
}

impl BLAKE2BPublicData<Target> {
    pub fn add_virtual<F: RichField + Extendable<D>, const D: usize>(
        builder: &mut CircuitBuilder<F, D>,
        digest: Target,
        chunk_size: usize,
    ) -> Self {
        let public_msg_chunk_targets = (0..16 * 1024)
            .map(|_| builder.add_virtual_target_arr::<4>())
            .collect::<Vec<_>>();

        let mut end_bits_targets = Vec::new();
        let mut hash_state_targets = Vec::new();

        for (digest, chunk_size) in digests.chunks_exact(32).zip_eq(chunk_sizes.iter()) {
            end_bits_targets.extend((0..(chunk_size - 1)).map(|_| builder.zero()));
            end_bits_targets.push(builder.one());

            hash_state_targets
                .extend((0..8 * (chunk_size - 1)).map(|_| builder.add_virtual_target_arr::<4>()));

            // Convert digest to little endian u32 chunks
            let u32_digest = digest.chunks_exact(4).map(|arr| {
                let mut array: [Target; 4] = arr.try_into().unwrap();
                array.reverse();
                array
            });
            hash_state_targets.extend(u32_digest);
        }

        BLAKE2BPublicData {
            hash_state: hash_state_targets,
            end_bits: end_bits_targets,
        }
    }

    pub fn public_input_targets<F: RichField + Extendable<D>, const D: usize>(
        &self,
        builder: &mut CircuitBuilder<F, D>,
    ) -> Vec<Target> {
        self.public_w
            .iter()
            .flatten()
            .copied()
            .chain(
                INITIAL_HASH
                    .map(|value| u32_to_le_field_bytes(value).map(|x| builder.constant(x)))
                    .into_iter()
                    .flatten(),
            )
            .chain(
                ROUND_CONSTANTS
                    .map(|value| u32_to_le_field_bytes(value).map(|x| builder.constant(x)))
                    .into_iter()
                    .flatten(),
            )
            .chain(self.hash_state.iter().flatten().copied())
            .chain(self.end_bits.iter().copied())
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct BLAKE2BGenerator<F: PrimeField64, E: CubicParameters<F>> {
    pub gadget: BLAKE2BGadget,
    pub table: ByteLookupTable,
    pub padded_messages: Vec<Target>,
    pub chunk_sizes: Vec<usize>,
    pub trace_generator: ArithmeticGenerator<BLAKE2BAirParameters<F, E>>,
    pub pub_values_target: BLAKE2BPublicData<Target>,
}

#[derive(Debug, Clone)]
pub struct BLAKE2BHintGenerator {
    padded_message: Vec<Target>,
    message_len: Target,
    digest_bytes: [Target; 32],
}

impl BLAKE2BHintGenerator {
    pub fn new(padded_message: &[Target], message_len: Target, digest_bytes: [Target; 32]) -> Self {
        BLAKE2BHintGenerator {
            padded_message: padded_message.to_vec(),
            message_len,
            digest_bytes,
        }
    }
}

impl BLAKE2BHintGenerator {
    pub fn id() -> String {
        "BLAKE2BHintGenerator".to_string()
    }
}

impl<F: RichField + Extendable<D>, const D: usize> SimpleGenerator<F, D> for BLAKE2BHintGenerator {
    fn id(&self) -> String {
        Self::id()
    }

    fn dependencies(&self) -> Vec<Target> {
        self.padded_message.clone()
    }

    fn serialize(
        &self,
        dst: &mut Vec<u8>,
        _: &CommonCircuitData<F, D>,
    ) -> plonky2::util::serialization::IoResult<()> {
        dst.write_target_vec(&self.padded_message)?;
        dst.write_target(self.message_len)?;
        dst.write_target_vec(&self.digest_bytes)?;
        Ok(())
    }

    fn deserialize(
        src: &mut Buffer,
        _: &CommonCircuitData<F, D>,
    ) -> plonky2::util::serialization::IoResult<Self>
    where
        Self: Sized,
    {
        let padded_message = src.read_target_vec()?;
        let message_len = src.read_target()?;
        let digest_bytes = src.read_target_vec()?;
        Ok(Self {
            padded_message,
            message_len,
            digest_bytes: digest_bytes.try_into().unwrap(),
        })
    }

    fn run_once(&self, witness: &PartitionWitness<F>, out_buffer: &mut GeneratedValues<F>) {
        let padded_message = witness
            .get_targets(&self.padded_message)
            .into_iter()
            .map(|x| x.as_canonical_u64() as u8)
            .collect::<Vec<_>>();

        let message_len = witness.get_target(self.message_len).as_canonical_u64() as usize;

        let mut state: [u64; 8] = [0; 8];
        state[..8].copy_from_slice(&INITIAL_HASH[..8]);

        let num_chunks = padded_message.len() / 128;
        let mut bytes_compressed = 0;
        assert!(padded_message.len() % 128 == 0);
        for (chunk_num, chunk) in padded_message.chunks_exact(128).enumerate() {
            let last_chunk = chunk_num == num_chunks - 1;

            if last_chunk {
                bytes_compressed = message_len;
            } else {
                bytes_compressed += 128;
            }

            state = BLAKE2BGadget::compress(
                chunk.try_into().unwrap(),
                &mut state,
                bytes_compressed,
                last_chunk,
            );
        }

        // We only support a digest of 32 bytes.  Retrieve the first four elements of the state
        let binding = state[0..4]
            .iter()
            .flat_map(|x| u64_to_le_field_bytes::<F>(*x))
            .collect_vec();
        let digest_bytes = binding.as_slice();

        out_buffer.set_target_arr(&self.digest_bytes, digest_bytes);
    }
}
